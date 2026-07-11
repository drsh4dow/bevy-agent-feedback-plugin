import * as assert from "node:assert/strict";
import * as fs from "node:fs";
import * as net from "node:net";
import * as os from "node:os";
import * as path from "node:path";
import { test } from "node:test";
import {
  BevyFeedbackClient,
  BevyFeedbackError,
  type JsonObject,
  type Predicate,
} from "../clients/typescript/bevy_feedback.ts";

test("protocol mismatch explains how to synchronize client and game", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "bevy-feedback-ts-mismatch-"));
  const protocol = path.join(root, "agent.json");
  fs.writeFileSync(protocol, JSON.stringify({ protocol: "bevy-agent-feedback/0.4" }));

  assert.throws(
    () => new BevyFeedbackClient({ protocolFile: protocol }),
    /protocol_version_mismatch.*upgrade or downgrade.*same 0\.5 release/,
  );
  fs.rmSync(root, { recursive: true });
});

type Fixture = {
  client: BevyFeedbackClient;
  requests: JsonObject[];
  close: () => Promise<void>;
};

async function fixture(
  respond: (request: JsonObject) => JsonObject,
): Promise<Fixture> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "bevy-feedback-ts-"));
  const requests: JsonObject[] = [];
  const server = net.createServer((socket) => {
    socket.setEncoding("utf8");
    let buffer = "";
    socket.on("data", (chunk) => {
      buffer += chunk;
      for (;;) {
        const newline = buffer.indexOf("\n");
        if (newline < 0) break;
        const line = buffer.slice(0, newline).trim();
        buffer = buffer.slice(newline + 1);
        if (!line) continue;
        const request = JSON.parse(line) as JsonObject;
        requests.push(request);
        socket.write(`${JSON.stringify(respond(request))}\n`);
      }
    });
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  assert.ok(address && typeof address !== "string");
  const heartbeat = path.join(root, "heartbeat");
  fs.writeFileSync(heartbeat, Date.now().toString());
  const protocol = path.join(root, "agent.json");
  fs.writeFileSync(
    protocol,
    JSON.stringify({
      protocol: "bevy-agent-feedback/0.5",
      socket_addr: `127.0.0.1:${address.port}`,
      pid: process.pid,
      heartbeat_file: heartbeat,
      stale_after_ms: 10_000,
      deterministic_time: false,
      max_wait_frames: 7,
      max_abort_predicates: 2,
      max_time_advance_steps: 4,
      max_time_advance_seconds: 3,
    }),
  );
  const client = new BevyFeedbackClient({ protocolFile: protocol, timeoutMs: 1_000 });
  return {
    client,
    requests,
    close: async () => {
      await client.close();
      await new Promise<void>((resolve) => server.close(() => resolve()));
      fs.rmSync(root, { recursive: true, force: true });
    },
  };
}

function ok(request: JsonObject): JsonObject {
  return { id: request.id ?? null, ok: true, result: { status: "ok" } };
}

function captureResponse(request: JsonObject): JsonObject {
  const window = {
    logical_width: 640,
    logical_height: 480,
    physical_width: 640,
    physical_height: 480,
    scale_factor: 1,
    focused: true,
    visible: true,
    mode: "windowed",
  };
  const capture = {
    sequence: 1,
    path: "/captures/semantic-wait-failure.png",
    label: "semantic-wait-failure",
    requested_frame: 42,
    completed_frame: 43,
    image_width: 640,
    image_height: 480,
    window_at_request: window,
    window_at_completion: window,
    completion: "screenshot_captured",
  };
  return {
    id: request.id ?? null,
    ok: true,
    result: { status: "captured", capture, latest_capture: capture },
  };
}

const success: Predicate = {
  type: "state_equals",
  state: "GamePhase",
  value: "Playing",
};
const abort: Predicate = {
  type: "state_equals",
  state: "GamePhase",
  value: "LoadFailed",
};

test("capabilities are immutable and oversized waits send nothing", async () => {
  const game = await fixture(ok);
  assert.equal(game.client.capabilities.maxWaitFrames, 7);
  assert.equal(game.client.maxWaitFrames, 7);
  assert.ok(Object.isFrozen(game.client.capabilities));
  assert.throws(
    () => Object.assign(game.client.capabilities, { maxWaitFrames: 8 }),
    /read only|Cannot assign/,
  );
  assert.throws(
    () => game.client.waitFrames(8),
    /frames=8 exceeds server limit 7;.*explicit bounded requests/,
  );
  assert.deepEqual(game.requests, []);
  await game.close();
});

test("abort predicates serialize and attach a failure capture", async () => {
  const game = await fixture((request) => {
    if (request.command === "wait_for") {
      return {
        id: request.id ?? null,
        ok: false,
        error: {
          code: "predicate_aborted",
          message: "abort predicate matched",
          context: { snapshot: { frame: 42 } },
        },
      };
    }
    return request.command === "capture" ? captureResponse(request) : ok(request);
  });

  await assert.rejects(
    game.client.waitFor(success, 3, [abort]),
    (error: unknown) => {
      assert.ok(error instanceof BevyFeedbackError);
      assert.equal(error.code, "predicate_aborted");
      assert.equal(
        (error.context?.failure_capture as JsonObject | undefined)?.path,
        "/captures/semantic-wait-failure.png",
      );
      return true;
    },
  );
  assert.equal(game.requests[0]?.command, "wait_for");
  assert.deepEqual(game.requests[0]?.abort_predicates, [abort]);
  assert.equal(game.requests[1]?.command, "capture");
  assert.equal(game.requests[1]?.label, "semantic-wait-failure");
  await game.close();
});

test("capture failure preserves the semantic error", async () => {
  const game = await fixture((request) => {
    if (request.command === "wait_for") {
      return {
        id: request.id ?? null,
        ok: false,
        error: { code: "predicate_timeout", message: "deadline" },
      };
    }
    if (request.command === "capture") {
      return {
        id: request.id ?? null,
        ok: false,
        error: { code: "capture_failed", message: "no renderer" },
      };
    }
    return ok(request);
  });

  await assert.rejects(game.client.waitFor(success, 3), (error: unknown) => {
    assert.ok(error instanceof BevyFeedbackError);
    assert.equal(error.code, "predicate_timeout");
    assert.equal(error.context?.failure_capture, undefined);
    return true;
  });
  await game.close();
});
