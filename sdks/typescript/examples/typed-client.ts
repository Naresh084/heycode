import {
  APP_SERVER_PROTOCOL_VERSION,
  HeycodeClient,
  type HeycodeTransport,
} from "../src/client.ts";

class ExampleTransport implements HeycodeTransport {
  #cancel!: () => void;
  readonly #cancelled = new Promise<void>(resolve => { this.#cancel = resolve; });

  async exchange(
    raw: string,
    onNotification: (raw: string) => Promise<void>,
  ): Promise<string> {
    const request = JSON.parse(raw) as { id: number; method: string };
    let result: unknown;
    switch (request.method) {
      case "initialize":
        result = {
          protocolVersion: APP_SERVER_PROTOCOL_VERSION,
          server: { name: "heycode", version: "0.1.0" },
          capabilities: {
            turns: true, attachments: true, cancel: true, authorization: false,
            models: false, mcp: false, plugins: false, settings: false,
          },
        };
        break;
      case "session/open":
        result = { sessionId: "example-session", runtimeId: "native", cwd: "/workspace" };
        break;
      case "turn/start":
        await onNotification(JSON.stringify({
          jsonrpc: "2.0", method: "session/event",
          params: {
            sessionId: "example-session", sequence: 1,
            event: { type: "turn_started", turn_id: "1" },
          },
        }));
        await this.#cancelled;
        await onNotification(JSON.stringify({
          jsonrpc: "2.0", method: "session/event",
          params: {
            sessionId: "example-session", sequence: 2,
            event: { type: "turn_finished", turn_id: "1", reason: "cancelled" },
          },
        }));
        result = { turnId: "1", reason: "cancelled" };
        break;
      case "turn/cancel":
        this.#cancel();
        result = null;
        break;
      default:
        throw new Error("unsupported example method");
    }
    return JSON.stringify({ jsonrpc: "2.0", id: request.id, result });
  }
}

const client = new HeycodeClient(new ExampleTransport());
const session = await client.start();
await client.resume(session.sessionId);
const events: string[] = [];
const turn = client.turn("wait", [], event => { events.push(event.params.event.type); });
while (events.length === 0) await new Promise(resolve => setTimeout(resolve, 0));
await client.cancel();
const result = await turn;
if (result.reason !== "cancelled" || events.join(",") !== "turn_started,turn_finished") {
  throw new Error("typed example did not settle exactly");
}
