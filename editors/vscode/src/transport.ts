import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { isAbsolute } from "node:path";

import { HeycodeError, type HeycodeTransport } from "@heycode/sdk";

const MAX_APP_FRAME_BYTES = 4 * 1024 * 1024;
const MAX_TRANSPORT_FRAME_BYTES = MAX_APP_FRAME_BYTES + 1024;
const MAX_PENDING_OPERATIONS = 64;

export interface StdioAppTransportOptions {
  executable: string;
  args: string[];
  cwd: string;
  env?: NodeJS.ProcessEnv;
}

interface PendingOperation {
  readonly onNotification: (notification: string) => Promise<void>;
  readonly signal: AbortSignal | undefined;
  readonly abort: (() => void) | undefined;
  notificationTail: Promise<void>;
  responseSeen: boolean;
  readonly resolve: (response: string) => void;
  readonly reject: (error: HeycodeError) => void;
}

interface TransportEnvelope {
  kind: "notification" | "response";
  operation: number;
  notification?: unknown;
  response?: unknown;
}

/**
 * One same-user local transport backed by a directly spawned heycode child.
 *
 * The process is never launched through a shell. Its stdout is reserved for
 * bounded protocol frames and stderr is drained without being retained or
 * rendered. This class never logs a raw frame.
 */
export class StdioAppTransport implements HeycodeTransport {
  readonly #child: ChildProcessWithoutNullStreams;
  readonly #pending = new Map<number, PendingOperation>();
  #lastOperation = 0;
  #writeTail: Promise<void> = Promise.resolve();
  #lineParts: Buffer[] = [];
  #lineBytes = 0;
  #closed = false;
  #didExit = false;
  #disposing: Promise<void> | null = null;
  readonly #exited: Promise<void>;

  constructor(options: StdioAppTransportOptions) {
    validateOptions(options);
    this.#child = spawn(options.executable, options.args, {
      cwd: options.cwd,
      env: options.env,
      shell: false,
      stdio: ["pipe", "pipe", "pipe"],
      windowsHide: true,
    });
    this.#exited = new Promise(resolve => {
      this.#child.once("exit", () => {
        this.#didExit = true;
        this.#closed = true;
        this.#failAll(new HeycodeError("unavailable"));
        resolve();
      });
    });
    this.#child.once("error", () => this.#fail(new HeycodeError("unavailable")));
    this.#child.stdout.on("data", (chunk: Buffer) => this.#acceptStdout(chunk));
    this.#child.stdout.once("end", () => {
      if (this.#lineBytes !== 0) this.#fail(new HeycodeError("invalid_request"));
    });
    this.#child.stderr.on("data", () => undefined);
  }

  get closed(): boolean {
    return this.#closed;
  }

  async exchange(
    request: string,
    onNotification: (notification: string) => Promise<void>,
    signal?: AbortSignal,
  ): Promise<string> {
    if (this.#closed) throw new HeycodeError("closed");
    if (signal?.aborted === true) throw new HeycodeError("cancelled");
    assertFrameBound(request, MAX_APP_FRAME_BYTES);
    const parsed = parseRecord(request);
    const operation = parsed.id;
    if (!Number.isSafeInteger(operation) || typeof operation !== "number" || operation <= 0) {
      throw new HeycodeError("invalid_request");
    }
    if (
      this.#pending.size >= MAX_PENDING_OPERATIONS ||
      this.#pending.has(operation) ||
      operation <= this.#lastOperation
    ) {
      throw new HeycodeError("conflict");
    }
    this.#lastOperation = operation;

    const operationSignal = signal;
    let settle: PendingOperation | undefined;
    const response = new Promise<string>((resolve, reject) => {
      const abort = operationSignal === undefined
        ? undefined
        : () => {
            void this.#write({ kind: "cancel", operation }).catch(() => undefined);
          };
      settle = {
        onNotification,
        signal: operationSignal,
        abort,
        notificationTail: Promise.resolve(),
        responseSeen: false,
        resolve,
        reject,
      };
      this.#pending.set(operation, settle);
      if (abort !== undefined) operationSignal?.addEventListener("abort", abort, { once: true });
    });
    try {
      await this.#write({ kind: "request", operation, request: parsed });
    } catch {
      const error = new HeycodeError("unavailable");
      this.#fail(error);
      if (settle !== undefined) this.#settle(operation, settle, error);
    }
    return response;
  }

  async dispose(): Promise<void> {
    if (this.#disposing !== null) return this.#disposing;
    this.#disposing = this.#dispose();
    return this.#disposing;
  }

  async #dispose(): Promise<void> {
    if (!this.#closed) {
      for (const operation of this.#pending.keys()) {
        await this.#write({ kind: "cancel", operation }).catch(() => undefined);
      }
      await this.#writeTail.catch(() => undefined);
      this.#child.stdin.end();
      let timer: NodeJS.Timeout | undefined;
      const settled = await Promise.race([
        this.#exited.then(() => true),
        new Promise<boolean>(resolve => {
          timer = setTimeout(() => resolve(false), 5_000);
        }),
      ]);
      if (timer !== undefined) clearTimeout(timer);
      if (!settled) {
        this.#child.kill();
        await this.#exited;
      }
    } else if (!this.#didExit) {
      this.#child.kill();
      await this.#exited;
    }
    this.#closed = true;
    this.#failAll(new HeycodeError("closed"));
  }

  #acceptStdout(chunk: Buffer): void {
    if (this.#closed) return;
    let start = 0;
    while (start < chunk.length) {
      const newline = chunk.indexOf(0x0a, start);
      const end = newline === -1 ? chunk.length : newline;
      const part = chunk.subarray(start, end);
      if (this.#lineBytes + part.length > MAX_TRANSPORT_FRAME_BYTES) {
        this.#fail(new HeycodeError("invalid_request"));
        return;
      }
      if (part.length !== 0) {
        this.#lineParts.push(part);
        this.#lineBytes += part.length;
      }
      if (newline === -1) return;
      let raw: string;
      try {
        raw = new TextDecoder("utf-8", { fatal: true }).decode(
          Buffer.concat(this.#lineParts, this.#lineBytes),
        );
      } catch {
        this.#fail(new HeycodeError("invalid_request"));
        return;
      }
      this.#lineParts = [];
      this.#lineBytes = 0;
      if (raw.length === 0) {
        this.#fail(new HeycodeError("invalid_request"));
        return;
      }
      void this.#dispatch(raw).catch(() => this.#fail(new HeycodeError("invalid_request")));
      start = newline + 1;
    }
  }

  async #dispatch(raw: string): Promise<void> {
    assertFrameBound(raw, MAX_TRANSPORT_FRAME_BYTES);
    const value = parseRecord(raw) as unknown as TransportEnvelope;
    if (
      (value.kind !== "notification" && value.kind !== "response") ||
      !Number.isSafeInteger(value.operation) || value.operation <= 0
    ) {
      throw new HeycodeError("invalid_request");
    }
    const pending = this.#pending.get(value.operation);
    if (pending === undefined) throw new HeycodeError("invalid_request");
    if (value.kind === "notification") {
      assertKeys(value as unknown as Record<string, unknown>, ["kind", "operation", "notification"]);
      if (pending.responseSeen || value.notification === undefined || value.response !== undefined) {
        throw new HeycodeError("invalid_request");
      }
      const notification = JSON.stringify(value.notification);
      assertFrameBound(notification, MAX_APP_FRAME_BYTES);
      pending.notificationTail = pending.notificationTail.then(() => pending.onNotification(notification));
      await pending.notificationTail;
      return;
    }
    assertKeys(value as unknown as Record<string, unknown>, ["kind", "operation", "response"]);
    if (pending.responseSeen || value.response === undefined || value.notification !== undefined) {
      throw new HeycodeError("invalid_request");
    }
    pending.responseSeen = true;
    await pending.notificationTail;
    const response = JSON.stringify(value.response);
    assertFrameBound(response, MAX_APP_FRAME_BYTES);
    this.#settle(
      value.operation,
      pending,
      pending.signal?.aborted === true ? new HeycodeError("cancelled") : response,
    );
  }

  #settle(operation: number, pending: PendingOperation, outcome: string | HeycodeError): void {
    if (this.#pending.get(operation) !== pending) return;
    this.#pending.delete(operation);
    if (pending.abort !== undefined) {
      pending.signal?.removeEventListener("abort", pending.abort);
    }
    if (typeof outcome === "string") pending.resolve(outcome);
    else pending.reject(outcome);
  }

  #write(frame: unknown): Promise<void> {
    const raw = `${JSON.stringify(frame)}\n`;
    assertFrameBound(raw, MAX_TRANSPORT_FRAME_BYTES + 1);
    const write = this.#writeTail.then(async () => {
      if (this.#closed || this.#child.stdin.destroyed) throw new HeycodeError("closed");
      if (!this.#child.stdin.write(raw, "utf8")) {
        await new Promise<void>((resolve, reject) => {
          const onDrain = () => { cleanup(); resolve(); };
          const onError = () => { cleanup(); reject(new HeycodeError("unavailable")); };
          const cleanup = () => {
            this.#child.stdin.off("drain", onDrain);
            this.#child.stdin.off("error", onError);
          };
          this.#child.stdin.once("drain", onDrain);
          this.#child.stdin.once("error", onError);
        });
      }
    });
    this.#writeTail = write.catch(() => undefined);
    return write;
  }

  #fail(error: HeycodeError): void {
    if (!this.#closed) {
      this.#closed = true;
      this.#child.stdin.destroy();
      this.#child.kill();
    }
    this.#failAll(error);
  }

  #failAll(error: HeycodeError): void {
    for (const [operation, pending] of this.#pending) {
      this.#settle(operation, pending, error);
    }
  }
}

function validateOptions(options: StdioAppTransportOptions): void {
  if (!isAbsolute(options.executable) || !isAbsolute(options.cwd)) {
    throw new HeycodeError("invalid_request");
  }
  if (
    options.args.length > 128 ||
    options.args.some(argument => argument.length > 4096 || argument.includes("\0"))
  ) {
    throw new HeycodeError("invalid_request");
  }
}

function parseRecord(raw: string): Record<string, unknown> {
  let value: unknown;
  try {
    value = JSON.parse(raw) as unknown;
  } catch {
    throw new HeycodeError("invalid_request");
  }
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new HeycodeError("invalid_request");
  }
  return value as Record<string, unknown>;
}

function assertFrameBound(raw: string, maximum: number): void {
  const bytes = Buffer.byteLength(raw, "utf8");
  if (bytes === 0 || bytes > maximum) throw new HeycodeError("invalid_request");
}

function assertKeys(value: Record<string, unknown>, allowed: readonly string[]): void {
  const accepted = new Set(allowed);
  if (Object.keys(value).some(key => !accepted.has(key))) {
    throw new HeycodeError("invalid_request");
  }
}
