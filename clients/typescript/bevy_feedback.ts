import * as fs from "node:fs";
import * as net from "node:net";

const PROTOCOL_VERSION = "bevy-agent-feedback/3";
const DEFAULT_MAX_WAIT_FRAMES = 300;
const DEFAULT_MAX_ABORT_PREDICATES = 16;
const NANOSECONDS_PER_SECOND = 1_000_000_000n;
const MAX_CLIENT_CHUNKS = 4_096;
const MAX_RESPONSE_BUFFER_BYTES = 8 * 1024 * 1024;
const MAX_PENDING_REQUESTS = 1_024;

export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type JsonObject = { [key: string]: Json };
export type TargetKind = "any" | "ui" | "world";
export type ComparisonOperator = "eq" | "ne" | "lt" | "lte" | "gt" | "gte";
export type DiagnosticValue = null | boolean | number | string;

export type TargetSelector =
  | { name: string; accessibility_label?: never; marker?: never }
  | { name?: never; accessibility_label: string; marker?: never }
  | { name?: never; accessibility_label?: never; marker: string };

export type Predicate =
  | { type: "state_equals"; state: string; value: DiagnosticValue }
  | {
      type: "resource_field";
      resource: string;
      field: string;
      operator: ComparisonOperator;
      value: DiagnosticValue;
    }
  | { type: "marker_count"; marker: string; min?: number; max?: number }
  | { type: "target_exists"; target: TargetSelector; kind?: TargetKind; camera?: string }
  | { type: "target_absent"; target: TargetSelector; kind?: TargetKind; camera?: string };

export interface WindowInfo {
  logical_width: number;
  logical_height: number;
  physical_width: number;
  physical_height: number;
  scale_factor: number;
  cursor_position?: [number, number];
  focused: boolean;
  visible: boolean;
  mode: "windowed" | "borderless_fullscreen" | "fullscreen";
}

export interface CaptureInfo {
  sequence: number;
  path: string;
  label?: string;
  requested_frame: number;
  completed_frame: number;
  image_width: number;
  image_height: number;
  window_at_request: WindowInfo;
  window_at_completion?: WindowInfo;
  completion: "screenshot_captured";
}

export interface ObservedPredicate {
  predicate: Predicate;
  outcome: "matched" | "not_matched" | "indeterminate";
  value?: DiagnosticValue;
  count?: number;
  count_is_lower_bound?: boolean;
}

export interface BevyFeedbackConfig {
  protocolFile?: string;
  timeoutMs?: number;
  transcriptFile?: string;
}

export interface BevyFeedbackCapabilities {
  readonly maxWaitFrames: number;
  readonly maxAbortPredicates: number;
  readonly deterministicTime: boolean;
  readonly maxTimeAdvanceSteps: number;
  readonly maxTimeAdvanceSeconds: number;
}

export interface TargetOptions {
  kind?: TargetKind;
  camera?: string;
}

export interface ClickTargetOptions extends TargetOptions {
  button?: string;
  frames?: number;
}

export class BevyFeedbackError extends Error {
  readonly code?: string;
  context?: JsonObject;

  constructor(message: string, code?: string, context?: JsonObject) {
    super(message);
    this.name = "BevyFeedbackError";
    this.code = code;
    this.context = context;
  }
}

type PendingRequest = {
  request: JsonObject;
  tsMs: number;
  startedMs: number;
  timeout: NodeJS.Timeout;
  resolve: (response: JsonObject) => void;
  reject: (error: Error) => void;
};

export class BevyFeedbackClient {
  private readonly socket: net.Socket;
  private readonly ready: Promise<void>;
  private readonly timeoutMs: number;
  private readonly transcriptFile?: string;
  readonly capabilities: Readonly<BevyFeedbackCapabilities>;
  private buffer = "";
  private nextId = 1;
  private closed = false;
  private readonly pending = new Map<string, PendingRequest>();

  lastCaptureInfo?: CaptureInfo;
  lastObservation?: ObservedPredicate;

  constructor(config: BevyFeedbackConfig = {}) {
    const protocolFile =
      config.protocolFile ??
      process.env.BEVY_FEEDBACK_PROTOCOL ??
      "target/agent-feedback/agent-feedback.json";
    this.timeoutMs = positiveInteger("timeoutMs", config.timeoutMs ?? 10_000);
    this.transcriptFile = config.transcriptFile ?? process.env.BEVY_FEEDBACK_TRANSCRIPT;

    const protocol = readProtocol(protocolFile);
    if (typeof protocol.deterministic_time !== "boolean") {
      throw new BevyFeedbackError("protocol missing deterministic_time");
    }
    this.capabilities = Object.freeze({
      deterministicTime: protocol.deterministic_time,
      maxWaitFrames: protocolPositiveInteger(
        protocol,
        "max_wait_frames",
        DEFAULT_MAX_WAIT_FRAMES,
      ),
      maxAbortPredicates: protocolPositiveInteger(
        protocol,
        "max_abort_predicates",
        DEFAULT_MAX_ABORT_PREDICATES,
      ),
      maxTimeAdvanceSteps: protocolPositiveInteger(protocol, "max_time_advance_steps"),
      maxTimeAdvanceSeconds: protocolPositiveNumber(protocol, "max_time_advance_seconds"),
    });
    const { host, port } = parseSocketAddr(String(protocol.socket_addr));
    this.socket = net.createConnection({ host, port });
    this.socket.setEncoding("utf8");
    const ready = Promise.withResolvers<void>();
    this.ready = ready.promise;
    this.socket.once("connect", ready.resolve);
    this.socket.once("error", ready.reject);
    this.socket.on("data", (data) => this.receive(data));
    this.socket.on("close", () => this.rejectPending("agent socket closed"));
    this.socket.on("error", (error) => this.rejectPending(error.message));
  }

  get deterministicTime(): boolean {
    return this.capabilities.deterministicTime;
  }

  get maxWaitFrames(): number {
    return this.capabilities.maxWaitFrames;
  }

  get maxAbortPredicates(): number {
    return this.capabilities.maxAbortPredicates;
  }

  get maxTimeAdvanceSteps(): number {
    return this.capabilities.maxTimeAdvanceSteps;
  }

  get maxTimeAdvanceSeconds(): number {
    return this.capabilities.maxTimeAdvanceSeconds;
  }

  async request(request: JsonObject): Promise<JsonObject> {
    if (this.closed) {
      throw new BevyFeedbackError("client is closed");
    }
    await this.ready;

    if (this.pending.size >= MAX_PENDING_REQUESTS) {
      throw new BevyFeedbackError(`client has ${MAX_PENDING_REQUESTS} pending requests`);
    }
    if (!Number.isSafeInteger(this.nextId)) {
      throw new BevyFeedbackError("request id space exhausted");
    }
    const body: JsonObject = { ...request };
    if (body.id === undefined) {
      body.id = this.nextId;
      this.nextId += 1;
    }
    const key = JSON.stringify(body.id);
    if (this.pending.has(key)) {
      throw new BevyFeedbackError(`duplicate pending request id: ${key}`);
    }

    const line = JSON.stringify(body);
    const tsMs = Date.now();
    const startedMs = Date.now();
    const pendingResponse = Promise.withResolvers<JsonObject>();
    const timeout = setTimeout(() => {
      this.pending.delete(key);
      pendingResponse.reject(
        new BevyFeedbackError(
          this.formatContext(`agent request timed out after ${this.timeoutMs} ms`),
          "client_timeout",
          this.localContext(),
        ),
      );
    }, this.timeoutMs);
    this.pending.set(key, {
      request: body,
      tsMs,
      startedMs,
      timeout,
      resolve: pendingResponse.resolve,
      reject: pendingResponse.reject,
    });

    this.socket.write(line + "\n");
    return pendingResponse.promise;
  }

  async replayJsonl(path: string): Promise<JsonObject[]> {
    const lines = fs.readFileSync(path, "utf8").split(/\r?\n/);
    if (lines.length > MAX_CLIENT_CHUNKS) {
      throw new BevyFeedbackError(`replay exceeds ${MAX_CLIENT_CHUNKS} lines`);
    }
    const responses: JsonObject[] = [];
    for (const rawLine of lines) {
      const line = rawLine.trim();
      if (!line) {
        continue;
      }
      const value = JSON.parse(line) as Json;
      const request = isObject(value) && isObject(value.request) ? value.request : value;
      if (!isObject(request)) {
        throw new BevyFeedbackError(`replay line is not a request object: ${line}`);
      }
      responses.push(await this.request(request));
    }
    return responses;
  }

  waitFrames(frames = 1): Promise<JsonObject> {
    const requested = positiveInteger("frames", frames);
    waitLimit("frames", requested, this.maxWaitFrames);
    return this.request({ command: "wait", frames: requested });
  }

  waitSeconds(seconds: number, maxFrames?: number): Promise<JsonObject> {
    const request: JsonObject = {
      command: "wait_seconds",
      seconds: positiveNumber("seconds", seconds),
    };
    if (maxFrames !== undefined) {
      const requested = positiveInteger("maxFrames", maxFrames);
      waitLimit("maxFrames", requested, this.maxWaitFrames);
      request.max_frames = requested;
    }
    return this.request(request);
  }

  async advanceTime(seconds: number, stepSeconds?: number): Promise<JsonObject[]> {
    const totalNs = durationNanoseconds("seconds", seconds);
    const capNs = durationNanoseconds("max_time_advance_seconds", this.maxTimeAdvanceSeconds);
    const stepNs =
      stepSeconds === undefined ? undefined : durationNanoseconds("stepSeconds", stepSeconds);

    if (stepNs === undefined) {
      if (totalNs > capNs) {
        throw new BevyFeedbackError(
          "advanceTime requires explicit stepSeconds when chunking; the server's default nominal step is not discoverable",
        );
      }
      return [await this.request({ command: "advance_time", seconds: nanosecondsSeconds(totalNs) })];
    }

    const stepsByDuration = capNs / stepNs;
    const chunkSteps = bigintMin(BigInt(this.maxTimeAdvanceSteps), stepsByDuration);
    if (chunkSteps < 1n) {
      throw new BevyFeedbackError(
        "stepSeconds exceeds advertised max_time_advance_seconds",
      );
    }
    const chunkNs = chunkSteps * stepNs;
    const chunkCount = bigintCeilDiv(totalNs, chunkNs);
    boundedChunkCountBigInt(chunkCount);

    const responses: JsonObject[] = [];
    let remainingNs = totalNs;
    for (let index = 0; index < Number(chunkCount); index += 1) {
      const currentNs = bigintMin(remainingNs, chunkNs);
      responses.push(
        await this.request({
          command: "advance_time",
          seconds: nanosecondsSeconds(currentNs),
          step_seconds: nanosecondsSeconds(stepNs),
        }),
      );
      remainingNs -= currentNs;
    }
    return responses;
  }

  async capture(label?: string): Promise<string> {
    return this.recordCapture(
      await this.request(label === undefined ? { command: "capture" } : { command: "capture", label }),
    );
  }

  async captureAfterFrames(frames: number, label?: string): Promise<string> {
    const request: JsonObject = {
      command: "capture_after_frames",
      frames: boundedFrames("frames", frames, this.maxWaitFrames),
    };
    if (label !== undefined) {
      request.label = label;
    }
    return this.recordCapture(await this.request(request));
  }

  waitUntilFirstCapture(): Promise<string> {
    return this.captureAfterFrames(1);
  }

  windowInfo(): Promise<JsonObject> {
    return this.request({ command: "window_info" });
  }

  cursorMove(x: number, y: number): Promise<JsonObject> {
    return this.request({ command: "cursor_move", x, y });
  }

  keyDown(key: string): Promise<JsonObject> {
    return this.request({ command: "key_down", key });
  }

  keyUp(key: string): Promise<JsonObject> {
    return this.request({ command: "key_up", key });
  }

  mouseDown(button = "Left"): Promise<JsonObject> {
    return this.request({ command: "mouse_down", button });
  }

  mouseUp(button = "Left"): Promise<JsonObject> {
    return this.request({ command: "mouse_up", button });
  }

  click(x: number, y: number, button = "Left"): Promise<JsonObject> {
    return this.request({ command: "click", x, y, button });
  }

  drag(
    button: string,
    start: [number, number],
    end: [number, number],
    steps = 10,
    frames = steps,
  ): Promise<JsonObject> {
    return this.request({ command: "drag", button, from: start, to: end, steps, frames });
  }

  scroll(lines: number): Promise<JsonObject> {
    return this.request({ command: "scroll", lines });
  }

  keyTap(key: string): Promise<JsonObject> {
    return this.request({ command: "key_tap", key });
  }

  keyHold(key: string, frames: number): Promise<JsonObject> {
    return this.request({ command: "key_hold", key, frames });
  }

  targetInfo(target: TargetSelector, options: TargetOptions = {}): Promise<JsonObject> {
    return this.request(targetRequest("target_info", target, options));
  }

  waitForTarget(
    target: TargetSelector,
    options: TargetOptions & { maxFrames?: number } = {},
  ): Promise<ObservedPredicate> {
    return this.waitFor(targetPredicate("target_exists", target, options), options.maxFrames);
  }

  waitForTargetAbsent(
    target: TargetSelector,
    options: TargetOptions & { maxFrames?: number } = {},
  ): Promise<ObservedPredicate> {
    return this.waitFor(targetPredicate("target_absent", target, options), options.maxFrames);
  }

  clickTarget(target: TargetSelector, options: ClickTargetOptions = {}): Promise<JsonObject> {
    const request = targetRequest("click_target", target, options);
    request.button = options.button ?? "Left";
    request.frames = boundedFrames("frames", options.frames ?? 1, this.maxWaitFrames);
    return this.request(request);
  }

  clickNamed(name: string, options: ClickTargetOptions = {}): Promise<JsonObject> {
    return this.clickTarget({ name }, options);
  }

  clickAccessibilityLabel(label: string, options: ClickTargetOptions = {}): Promise<JsonObject> {
    return this.clickTarget({ accessibility_label: label }, options);
  }

  clickMarker(marker: string, options: ClickTargetOptions = {}): Promise<JsonObject> {
    return this.clickTarget({ marker }, options);
  }

  resourceInfo(resource?: string, field?: string): Promise<JsonObject> {
    const request: JsonObject = { command: "resource_info" };
    if (resource !== undefined) {
      request.resource = resource;
    }
    if (field !== undefined) {
      request.field = field;
    }
    return this.request(request);
  }

  async readResourceField(resource: string, field: string): Promise<Json> {
    const response = await this.resourceInfo(resource, field);
    const details = responseDetails(response);
    if (!("value" in details)) {
      throw new BevyFeedbackError(`resource_info response missing field value: ${JSON.stringify(response)}`);
    }
    return details.value;
  }

  async evaluatePredicate(predicate: Predicate): Promise<ObservedPredicate> {
    validatePredicate(predicate);
    return this.recordObservation(
      await this.request({ command: "evaluate_predicate", predicate: predicate as unknown as Json }),
    );
  }

  async waitFor(
    predicate: Predicate,
    maxFrames?: number,
    abortPredicates: readonly Predicate[] = [],
  ): Promise<ObservedPredicate> {
    validatePredicate(predicate);
    if (abortPredicates.length > this.maxAbortPredicates) {
      throw new BevyFeedbackError(
        `abortPredicates has ${abortPredicates.length} items, but server supports ${this.maxAbortPredicates}; reduce abort predicates or configure separate explicit waits`,
      );
    }
    for (const abortPredicate of abortPredicates) {
      validatePredicate(abortPredicate);
    }
    const request: JsonObject = {
      command: "wait_for",
      predicate: predicate as unknown as Json,
    };
    if (abortPredicates.length > 0) {
      request.abort_predicates = abortPredicates as unknown as Json;
    }
    if (maxFrames !== undefined) {
      const requested = positiveInteger("maxFrames", maxFrames);
      waitLimit("maxFrames", requested, this.maxWaitFrames);
      request.max_frames = requested;
    }
    try {
      return this.recordObservation(await this.request(request));
    } catch (error) {
      if (
        error instanceof BevyFeedbackError &&
        (error.code === "predicate_timeout" || error.code === "predicate_aborted")
      ) {
        try {
          await this.capture("semantic-wait-failure");
          if (this.lastCaptureInfo !== undefined) {
            error.context = {
              ...(error.context ?? {}),
              failure_capture: this.lastCaptureInfo as unknown as Json,
            };
            error.message += `; failure_capture=${JSON.stringify(this.lastCaptureInfo)}`;
          }
        } catch {
          // Best effort: preserve the semantic error.
        }
      }
      throw error;
    }
  }

  waitForState(
    state: string,
    value: DiagnosticValue,
    maxFrames?: number,
    abortValues: readonly DiagnosticValue[] = [],
  ): Promise<ObservedPredicate> {
    if (abortValues.length > this.maxAbortPredicates) {
      throw new BevyFeedbackError(
        `abortValues has ${abortValues.length} items, but server supports ${this.maxAbortPredicates}; reduce abort values or configure separate explicit waits`,
      );
    }
    return this.waitFor(
      { type: "state_equals", state, value },
      maxFrames,
      abortValues.map((abortValue) => ({
        type: "state_equals",
        state,
        value: abortValue,
      })),
    );
  }

  waitForResource(
    resource: string,
    field: string,
    operator: ComparisonOperator,
    value: DiagnosticValue,
    maxFrames?: number,
  ): Promise<ObservedPredicate> {
    return this.waitFor({ type: "resource_field", resource, field, operator, value }, maxFrames);
  }

  waitForMarkerCount(
    marker: string,
    bounds: { min?: number; max?: number },
    maxFrames?: number,
  ): Promise<ObservedPredicate> {
    return this.waitFor(markerPredicate(marker, bounds), maxFrames);
  }

  waitForMarkerPresent(marker: string, maxFrames?: number): Promise<ObservedPredicate> {
    return this.waitForMarkerCount(marker, { min: 1 }, maxFrames);
  }

  waitForMarkerAbsent(marker: string, maxFrames?: number): Promise<ObservedPredicate> {
    return this.waitForMarkerCount(marker, { max: 0 }, maxFrames);
  }

  async assertPredicate(predicate: Predicate): Promise<ObservedPredicate> {
    const observed = await this.evaluatePredicate(predicate);
    if (observed.outcome !== "matched") {
      throw new BevyFeedbackError(
        `predicate assertion failed: ${JSON.stringify(observed)}`,
        "assertion_failed",
        { observed_predicate: observed as unknown as Json },
      );
    }
    return observed;
  }

  assertState(state: string, value: DiagnosticValue): Promise<ObservedPredicate> {
    return this.assertPredicate({ type: "state_equals", state, value });
  }

  assertResource(
    resource: string,
    field: string,
    operator: ComparisonOperator,
    value: DiagnosticValue,
  ): Promise<ObservedPredicate> {
    return this.assertPredicate({ type: "resource_field", resource, field, operator, value });
  }

  assertMarkerCount(
    marker: string,
    bounds: { min?: number; max?: number },
  ): Promise<ObservedPredicate> {
    return this.assertPredicate(markerPredicate(marker, bounds));
  }

  assertMarkerPresent(marker: string): Promise<ObservedPredicate> {
    return this.assertMarkerCount(marker, { min: 1 });
  }

  assertMarkerAbsent(marker: string): Promise<ObservedPredicate> {
    return this.assertMarkerCount(marker, { max: 0 });
  }

  assertTargetExists(target: TargetSelector, options: TargetOptions = {}): Promise<ObservedPredicate> {
    return this.assertPredicate(targetPredicate("target_exists", target, options));
  }

  assertTargetAbsent(target: TargetSelector, options: TargetOptions = {}): Promise<ObservedPredicate> {
    return this.assertPredicate(targetPredicate("target_absent", target, options));
  }

  releaseAllInputs(): Promise<JsonObject> {
    return this.request({ command: "release_all_inputs" });
  }

  shutdown(): Promise<JsonObject> {
    return this.request({ command: "shutdown" });
  }

  async close(): Promise<void> {
    if (this.closed) {
      return;
    }
    try {
      await this.releaseAllInputs();
    } catch {
      // Best effort: closing the socket must still proceed.
    }
    this.closed = true;
    this.socket.end();
  }

  private receive(data: string | Buffer): void {
    this.buffer += data.toString();
    if (Buffer.byteLength(this.buffer, "utf8") > MAX_RESPONSE_BUFFER_BYTES) {
      this.rejectPending(`response buffer exceeds ${MAX_RESPONSE_BUFFER_BYTES} bytes`);
      this.socket.destroy();
      return;
    }
    const lineCount = this.buffer.split("\n").length - 1;
    if (lineCount > MAX_CLIENT_CHUNKS) {
      this.rejectPending(`response batch exceeds ${MAX_CLIENT_CHUNKS} lines`);
      this.socket.destroy();
      return;
    }
    for (let index = 0; index < lineCount; index += 1) {
      const newline = this.buffer.indexOf("\n");
      const line = this.buffer.slice(0, newline).trim();
      this.buffer = this.buffer.slice(newline + 1);
      if (line) {
        this.receiveLine(line);
      }
    }
  }

  private receiveLine(line: string): void {
    let response: JsonObject;
    try {
      const parsed = JSON.parse(line) as Json;
      if (!isObject(parsed)) {
        throw new Error("response is not an object");
      }
      response = parsed;
    } catch (error) {
      this.rejectPending(`invalid JSON response: ${error}`);
      return;
    }

    const key = JSON.stringify(response.id);
    const pending = this.pending.get(key);
    if (!pending) {
      return;
    }
    this.pending.delete(key);
    clearTimeout(pending.timeout);
    this.writeTranscript(pending, response);
    this.recordResponseContext(response);

    if (response.ok === true) {
      pending.resolve(response);
      return;
    }
    const error = isObject(response.error) ? response.error : {};
    const code = typeof error.code === "string" ? error.code : "error";
    const rawMessage = typeof error.message === "string" ? error.message : JSON.stringify(response);
    const context = isObject(error.context) ? error.context : undefined;
    pending.reject(
      new BevyFeedbackError(
        this.formatContext(`command failed [${code}]: ${rawMessage}`, context),
        code,
        context,
      ),
    );
  }

  private recordCapture(response: JsonObject): string {
    const result = isObject(response.result) ? response.result : undefined;
    const capture = result && isObject(result.capture) ? result.capture : undefined;
    if (!capture || typeof capture.path !== "string") {
      throw new BevyFeedbackError(`capture response missing metadata: ${JSON.stringify(response)}`);
    }
    this.lastCaptureInfo = capture as unknown as CaptureInfo;
    return capture.path;
  }

  private recordObservation(response: JsonObject): ObservedPredicate {
    const details = responseDetails(response);
    if (typeof details.outcome !== "string" || !isObject(details.predicate)) {
      throw new BevyFeedbackError(
        `predicate response missing observation: ${JSON.stringify(response)}`,
      );
    }
    const observation = details as unknown as ObservedPredicate;
    this.lastObservation = observation;
    return observation;
  }

  private recordResponseContext(response: JsonObject): void {
    const result = isObject(response.result) ? response.result : undefined;
    if (result && isObject(result.latest_capture) && typeof result.latest_capture.path === "string") {
      this.lastCaptureInfo = result.latest_capture as unknown as CaptureInfo;
    }
    if (result && isObject(result.details) && typeof result.details.outcome === "string") {
      this.lastObservation = result.details as unknown as ObservedPredicate;
    }
    const error = isObject(response.error) ? response.error : undefined;
    const context = error && isObject(error.context) ? error.context : undefined;
    if (context && isObject(context.latest_capture) && typeof context.latest_capture.path === "string") {
      this.lastCaptureInfo = context.latest_capture as unknown as CaptureInfo;
    }
    if (context && isObject(context.observed_predicate)) {
      this.lastObservation = context.observed_predicate as unknown as ObservedPredicate;
    }
  }

  private writeTranscript(pending: PendingRequest, response: JsonObject): void {
    if (!this.transcriptFile) {
      return;
    }
    fs.appendFileSync(
      this.transcriptFile,
      JSON.stringify({
        ts_ms: pending.tsMs,
        duration_ms: Date.now() - pending.startedMs,
        request: pending.request,
        response,
      }) + "\n",
      "utf8",
    );
  }

  private rejectPending(message: string): void {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timeout);
      pending.reject(
        new BevyFeedbackError(this.formatContext(message), "connection_error", this.localContext()),
      );
    }
    this.pending.clear();
  }

  private localContext(): JsonObject | undefined {
    const context: JsonObject = {};
    if (this.lastCaptureInfo !== undefined) {
      context.latest_capture = this.lastCaptureInfo as unknown as Json;
    }
    if (this.lastObservation !== undefined) {
      context.observed_predicate = this.lastObservation as unknown as Json;
    }
    return Object.keys(context).length === 0 ? undefined : context;
  }

  private formatContext(message: string, context = this.localContext()): string {
    return context === undefined ? message : `${message}; context: ${JSON.stringify(context)}`;
  }
}

function readProtocol(path: string): JsonObject {
  const protocol = JSON.parse(fs.readFileSync(path, "utf8")) as Json;
  if (!isObject(protocol)) {
    throw new BevyFeedbackError(`protocol file is not an object: ${path}`);
  }
  if (protocol.protocol !== PROTOCOL_VERSION) {
    throw new BevyFeedbackError(`unsupported protocol ${String(protocol.protocol)}; expected ${PROTOCOL_VERSION}`);
  }
  const pid = typeof protocol.pid === "number" ? protocol.pid : 0;
  if (pid <= 0 || !processAlive(pid)) {
    throw new BevyFeedbackError(`protocol stale: process ${pid} is not alive`);
  }
  if (typeof protocol.heartbeat_file !== "string") {
    throw new BevyFeedbackError("protocol stale: missing heartbeat_file");
  }
  const heartbeatMs = Number(fs.readFileSync(protocol.heartbeat_file, "utf8").trim());
  const staleAfterMs = typeof protocol.stale_after_ms === "number" ? protocol.stale_after_ms : 0;
  const ageMs = Date.now() - heartbeatMs;
  if (!Number.isFinite(heartbeatMs) || ageMs > staleAfterMs) {
    throw new BevyFeedbackError(`protocol stale: heartbeat is ${ageMs} ms old, stale after ${staleAfterMs} ms`);
  }
  return protocol;
}

function parseSocketAddr(socketAddr: string): { host: string; port: number } {
  const split = socketAddr.lastIndexOf(":");
  if (split <= 0) {
    throw new BevyFeedbackError(`invalid socket address: ${socketAddr}`);
  }
  const port = Number(socketAddr.slice(split + 1));
  if (!Number.isInteger(port) || port <= 0) {
    throw new BevyFeedbackError(`invalid socket port: ${socketAddr}`);
  }
  return { host: socketAddr.slice(0, split), port };
}

function processAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function isObject(value: Json | undefined): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function positiveNumber(name: string, value: number): number {
  if (!Number.isFinite(value) || value <= 0) {
    throw new BevyFeedbackError(`${name} must be positive and finite`);
  }
  return value;
}

function positiveInteger(name: string, value: number): number {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new BevyFeedbackError(`${name} must be a positive safe integer`);
  }
  return value;
}

function boundedFrames(name: string, value: number, cap: number): number {
  const frames = positiveInteger(name, value);
  if (frames > cap) {
    throw new BevyFeedbackError(`${name} must not exceed advertised max_wait_frames ${cap}`);
  }
  return frames;
}

function waitLimit(name: string, requested: number, supported: number): void {
  if (requested > supported) {
    throw new BevyFeedbackError(
      `${name}=${requested} exceeds server limit ${supported}; configure AgentFeedbackConfig.max_wait_frames or issue explicit bounded requests`,
    );
  }
}

function protocolPositiveInteger(protocol: JsonObject, field: string, fallback?: number): number {
  const value = protocol[field] ?? fallback;
  if (typeof value !== "number") {
    throw new BevyFeedbackError(`protocol missing ${field}`);
  }
  return positiveInteger(field, value);
}

function protocolPositiveNumber(protocol: JsonObject, field: string): number {
  const value = protocol[field];
  if (typeof value !== "number") {
    throw new BevyFeedbackError(`protocol missing ${field}`);
  }
  return positiveNumber(field, value);
}

function durationNanoseconds(name: string, value: number): bigint {
  positiveNumber(name, value);
  if (value > Number.MAX_SAFE_INTEGER) {
    throw new BevyFeedbackError(`${name} is too large for exact nanosecond conversion`);
  }
  const text = value.toString().toLowerCase();
  const [coefficient, exponentText] = text.split("e");
  const exponent = exponentText === undefined ? 0 : Number(exponentText);
  const [whole, fraction = ""] = coefficient.split(".");
  const digits = BigInt(whole + fraction);
  const scale = 9 + exponent - fraction.length;
  let nanoseconds: bigint;
  if (scale >= 0) {
    nanoseconds = digits * 10n ** BigInt(scale);
  } else {
    const divisor = 10n ** BigInt(-scale);
    nanoseconds = (digits + divisor / 2n) / divisor;
  }
  if (nanoseconds <= 0n) {
    throw new BevyFeedbackError(`${name} is shorter than one nanosecond`);
  }
  return nanoseconds;
}

function nanosecondsSeconds(value: bigint): number {
  const whole = value / NANOSECONDS_PER_SECOND;
  const fraction = (value % NANOSECONDS_PER_SECOND).toString().padStart(9, "0");
  return Number(`${whole}.${fraction}`);
}

function bigintMin(left: bigint, right: bigint): bigint {
  return left < right ? left : right;
}

function bigintCeilDiv(numerator: bigint, denominator: bigint): bigint {
  return (numerator + denominator - 1n) / denominator;
}

function boundedChunkCount(count: number): void {
  if (!Number.isSafeInteger(count) || count < 1 || count > MAX_CLIENT_CHUNKS) {
    throw new BevyFeedbackError(`operation requires more than ${MAX_CLIENT_CHUNKS} chunks`);
  }
}

function boundedChunkCountBigInt(count: bigint): void {
  if (count < 1n || count > BigInt(MAX_CLIENT_CHUNKS)) {
    throw new BevyFeedbackError(`operation requires more than ${MAX_CLIENT_CHUNKS} chunks`);
  }
}

function targetRequest(
  command: "target_info" | "click_target",
  target: TargetSelector,
  options: TargetOptions,
): JsonObject {
  validateTarget(target);
  const request: JsonObject = { command, target: target as unknown as Json };
  if (options.kind !== undefined) {
    request.kind = options.kind;
  }
  if (options.camera !== undefined) {
    request.camera = options.camera;
  }
  return request;
}

function targetPredicate(
  type: "target_exists" | "target_absent",
  target: TargetSelector,
  options: TargetOptions,
): Predicate {
  validateTarget(target);
  const predicate: {
    type: "target_exists" | "target_absent";
    target: TargetSelector;
    kind?: TargetKind;
    camera?: string;
  } = { type, target };
  if (options.kind !== undefined) {
    predicate.kind = options.kind;
  }
  if (options.camera !== undefined) {
    predicate.camera = options.camera;
  }
  return predicate;
}

function validateTarget(target: TargetSelector): void {
  const count =
    Number(target.name !== undefined) +
    Number(target.accessibility_label !== undefined) +
    Number(target.marker !== undefined);
  const selected = target.name ?? target.accessibility_label ?? target.marker;
  if (count !== 1 || selected === undefined) {
    throw new BevyFeedbackError(
      "target selector must contain exactly one name, accessibility_label, or marker",
    );
  }
  const bytes = Buffer.byteLength(selected, "utf8");
  if (bytes < 1 || bytes > 128) {
    throw new BevyFeedbackError("target selector must contain 1..=128 UTF-8 bytes");
  }
}

function validatePredicate(predicate: Predicate): void {
  switch (predicate.type) {
    case "state_equals":
      validateDiagnosticValue(predicate.value);
      return;
    case "resource_field":
      validateDiagnosticValue(predicate.value);
      if (
        (predicate.operator === "lt" ||
          predicate.operator === "lte" ||
          predicate.operator === "gt" ||
          predicate.operator === "gte") &&
        typeof predicate.value !== "number"
      ) {
        throw new BevyFeedbackError("ordered resource comparison requires a numeric value");
      }
      return;
    case "marker_count":
      markerPredicate(predicate.marker, { min: predicate.min, max: predicate.max });
      return;
    case "target_exists":
    case "target_absent":
      validateTarget(predicate.target);
      return;
  }
}

function validateDiagnosticValue(value: DiagnosticValue): void {
  if (typeof value === "number" && !Number.isFinite(value)) {
    throw new BevyFeedbackError("diagnostic numeric values must be finite");
  }
  if (typeof value === "string") {
    const bytes = Buffer.byteLength(value, "utf8");
    if (bytes < 1 || bytes > 1_024) {
      throw new BevyFeedbackError("diagnostic strings must contain 1..=1024 UTF-8 bytes");
    }
  }
}

function markerPredicate(
  marker: string,
  bounds: { min?: number; max?: number },
): Predicate {
  if (bounds.min === undefined && bounds.max === undefined) {
    throw new BevyFeedbackError("marker count requires min or max");
  }
  if (
    bounds.min !== undefined &&
    (!Number.isSafeInteger(bounds.min) || bounds.min < 0 || bounds.min > 0xffff_ffff)
  ) {
    throw new BevyFeedbackError("marker min must be a u32 integer");
  }
  if (
    bounds.max !== undefined &&
    (!Number.isSafeInteger(bounds.max) || bounds.max < 0 || bounds.max > 0xffff_ffff)
  ) {
    throw new BevyFeedbackError("marker max must be a u32 integer");
  }
  return { type: "marker_count", marker, ...bounds };
}

function responseDetails(response: JsonObject): JsonObject {
  const result = isObject(response.result) ? response.result : undefined;
  const details = result && isObject(result.details) ? result.details : undefined;
  if (!details) {
    throw new BevyFeedbackError(`response missing result details: ${JSON.stringify(response)}`);
  }
  return details;
}
