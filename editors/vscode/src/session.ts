import {
  HeycodeClient,
  HeycodeError,
  type AppPermissionDecision,
  type AppServerNotification,
  type AppSessionInfo,
  type AppTurnResult,
} from "@heycode/sdk";

import { StdioAppTransport } from "./transport.js";

const MAX_HUMAN_REQUESTS_PER_TURN = 256;

export interface HeycodeSessionUi {
  onEvent(event: AppServerNotification): void | Promise<void>;
  choosePermission(
    request: Extract<AppServerNotification["params"]["event"], { type: "permission_requested" }>,
  ): Promise<AppPermissionDecision>;
  answerQuestion(
    request: Extract<AppServerNotification["params"]["event"], { type: "question_requested" }>,
  ): Promise<string | null>;
}

/** One extension-owned app-server host/session lifecycle. */
export class HeycodeSessionController {
  readonly #transport: StdioAppTransport;
  readonly #client: HeycodeClient;
  readonly #ui: HeycodeSessionUi;
  #activeTurn: Promise<AppTurnResult> | null = null;
  #cancelling: Promise<void> | null = null;
  #session: AppSessionInfo | null = null;
  #closing: Promise<void> | null = null;
  readonly #pendingHumanRequests = new Set<string>();
  readonly #settledHumanRequests = new Set<string>();

  constructor(transport: StdioAppTransport, ui: HeycodeSessionUi) {
    this.#transport = transport;
    this.#client = new HeycodeClient(transport);
    this.#ui = ui;
  }

  get session(): AppSessionInfo | null {
    return this.#session;
  }

  get turnActive(): boolean {
    return this.#activeTurn !== null;
  }

  async start(): Promise<AppSessionInfo> {
    const session = await this.#client.start();
    this.#session = session;
    return session;
  }

  async resume(expectedSessionId: string): Promise<AppSessionInfo> {
    const session = await this.#client.resume(expectedSessionId);
    this.#session = session;
    return session;
  }

  async send(text: string): Promise<AppTurnResult> {
    if (this.#session === null || this.#activeTurn !== null || text.length === 0) {
      throw new HeycodeError("conflict");
    }
    this.#pendingHumanRequests.clear();
    this.#settledHumanRequests.clear();
    const turn = this.#client.turn(text, [], event => this.#handleEvent(event));
    this.#activeTurn = turn;
    try {
      return await turn;
    } finally {
      if (this.#activeTurn === turn) this.#activeTurn = null;
      this.#pendingHumanRequests.clear();
    }
  }

  async cancel(): Promise<void> {
    if (this.#cancelling !== null) return this.#cancelling;
    const turn = this.#activeTurn;
    if (turn === null) return;
    const cancelling = this.#cancel(turn);
    this.#cancelling = cancelling;
    try {
      await cancelling;
    } finally {
      if (this.#cancelling === cancelling) this.#cancelling = null;
    }
  }

  async #cancel(turn: Promise<AppTurnResult>): Promise<void> {
    const cancellation = this.#client.cancel();
    await cancellation;
    try {
      await turn;
    } catch (error: unknown) {
      if (!(error instanceof HeycodeError) || error.code !== "cancelled") throw error;
    }
  }

  async close(): Promise<void> {
    if (this.#closing !== null) return this.#closing;
    this.#closing = this.#close();
    return this.#closing;
  }

  async dispose(): Promise<void> {
    return this.close();
  }

  async #close(): Promise<void> {
    if (this.#activeTurn !== null) await this.cancel().catch(() => undefined);
    if (this.#session !== null) {
      await this.#client.close().catch(() => undefined);
      this.#session = null;
    }
    await this.#transport.dispose();
  }

  async #handleEvent(notification: AppServerNotification): Promise<void> {
    await this.#ui.onEvent(notification);
    const event = notification.params.event;
    if (event.type === "permission_requested") {
      await this.#handlePermission(event);
    } else if (event.type === "question_requested") {
      await this.#handleQuestion(event);
    }
  }

  async #handlePermission(
    event: Extract<AppServerNotification["params"]["event"], { type: "permission_requested" }>,
  ): Promise<void> {
    this.#admitHumanRequest(event.request_id);
    let decision: AppPermissionDecision = "deny";
    try {
      decision = await this.#ui.choosePermission(event);
      if (decision !== "allow_once" && decision !== "allow_session" && decision !== "deny") {
        throw new HeycodeError("invalid_request");
      }
    } finally {
      await this.#client.respondPermission(event.request_id, decision);
      this.#settleHumanRequest(event.request_id);
    }
  }

  async #handleQuestion(
    event: Extract<AppServerNotification["params"]["event"], { type: "question_requested" }>,
  ): Promise<void> {
    this.#admitHumanRequest(event.request_id);
    const answer = await this.#ui.answerQuestion(event);
    if (answer === null) {
      await this.#client.cancel();
    } else {
      await this.#client.respondQuestion(event.request_id, answer);
    }
    this.#settleHumanRequest(event.request_id);
  }

  #admitHumanRequest(requestId: string): void {
    if (
      this.#pendingHumanRequests.size + this.#settledHumanRequests.size >=
        MAX_HUMAN_REQUESTS_PER_TURN ||
      this.#pendingHumanRequests.has(requestId) ||
      this.#settledHumanRequests.has(requestId)
    ) {
      throw new HeycodeError("invalid_request");
    }
    this.#pendingHumanRequests.add(requestId);
  }

  #settleHumanRequest(requestId: string): void {
    if (!this.#pendingHumanRequests.delete(requestId)) {
      throw new HeycodeError("invalid_request");
    }
    this.#settledHumanRequests.add(requestId);
  }
}
