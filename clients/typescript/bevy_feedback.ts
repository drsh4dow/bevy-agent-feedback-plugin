import * as fs from "node:fs";
import * as net from "node:net";

const PROTOCOL_VERSION = "bevy-agent-feedback/2";

type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
type JsonObject = { [key: string]: Json };

export interface BevyFeedbackConfig {
  protocolFile?: string;
  timeoutMs?: number;
  transcriptFile?: string;
}

export class BevyFeedbackError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "BevyFeedbackError";
  }
}

type PendingRequest = {
  request: JsonObject;
  tsMs: number;
  startedMs: number;
  timeout: ReturnType<typeof setTimeout>;
  resolve: (response: JsonObject) => void;
  reject: (error: Error) => void;
};

export class BevyFeedbackClient {
  private readonly socket: net.Socket;
  private readonly ready: Promise<void>;
  private readonly timeoutMs: number;
  private readonly transcriptFile?: string;
  private buffer = "";
  private nextId = 1;
  private closed = false;
  private lastCapture?: string;
  private readonly pending = new Map<string, PendingRequest>();

  constructor(config: BevyFeedbackConfig = {}) {
    const protocolFile =
      config.protocolFile ??
      process.env.BEVY_FEEDBACK_PROTOCOL ??
      "target/agent-feedback/agent-feedback.json";
    this.timeoutMs = config.timeoutMs ?? 10_000;
    this.transcriptFile = config.transcriptFile ?? process.env.BEVY_FEEDBACK_TRANSCRIPT;

    const protocol = readProtocol(protocolFile);
    const { host, port } = parseSocketAddr(String(protocol.socket_addr));
    this.socket = net.createConnection({ host, port });
    this.socket.setEncoding("utf8");
    this.ready = new Promise((resolve, reject) => {
      this.socket.once("connect", resolve);
      this.socket.once("error", reject);
    });
    this.socket.on("data", (data) => this.receive(data));
    this.socket.on("close", () => this.rejectPending("agent socket closed"));
    this.socket.on("error", (error) => this.rejectPending(error.message));
  }

  async request(request: JsonObject): Promise<JsonObject> {
    if (this.closed) {
      throw new BevyFeedbackError("client is closed");
    }
    await this.ready;

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
    const response = new Promise<JsonObject>((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.pending.delete(key);
        reject(new BevyFeedbackError(this.withLastCapture(`agent request timed out after ${this.timeoutMs} ms`)));
      }, this.timeoutMs);
      this.pending.set(key, { request: body, tsMs, startedMs, timeout, resolve, reject });
    });

    this.socket.write(line + "\n");
    return response;
  }

  async replayJsonl(path: string): Promise<JsonObject[]> {
    const responses: JsonObject[] = [];
    for (const rawLine of fs.readFileSync(path, "utf8").split(/\r?\n/)) {
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

  wait(frames = 1): Promise<JsonObject> {
    return this.request({ command: "wait", frames });
  }

  async capture(): Promise<string> {
    const response = await this.request({ command: "capture" });
    const capture = response.result as JsonObject | undefined;
    const info = capture?.capture as JsonObject | undefined;
    const path = info?.path;
    if (typeof path !== "string") {
      throw new BevyFeedbackError(`capture response missing path: ${JSON.stringify(response)}`);
    }
    this.lastCapture = path;
    return path;
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
      // best effort
    }
    this.closed = true;
    this.socket.end();
  }

  private receive(data: string | Buffer): void {
    this.buffer += data.toString();
    for (;;) {
      const newline = this.buffer.indexOf("\n");
      if (newline < 0) {
        return;
      }
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

    if (response.ok === true) {
      pending.resolve(response);
      return;
    }
    const error = isObject(response.error) ? response.error : {};
    const code = typeof error.code === "string" ? error.code : "error";
    const rawMessage = typeof error.message === "string" ? error.message : JSON.stringify(response);
    const message = code === "timeout" ? this.withLastCapture(rawMessage) : rawMessage;
    pending.reject(new BevyFeedbackError(`command failed [${code}]: ${message}`));
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
      pending.reject(new BevyFeedbackError(message));
    }
    this.pending.clear();
  }

  private withLastCapture(message: string): string {
    return this.lastCapture ? `${message}; last captured frame: ${this.lastCapture}` : message;
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
