// Backend IPC commands reject with a tagged CommandError ({ kind, message }) — see
// src-tauri/src/commands.rs. These helpers read it safely, falling back for plain
// string / Error rejections.

export type CommandErrorKind =
  | "validation"
  | "authentication"
  | "authorization"
  | "service"
  | "network";

export interface CommandError {
  kind: CommandErrorKind;
  message: string;
}

export function isCommandError(e: unknown): e is CommandError {
  return (
    typeof e === "object" &&
    e !== null &&
    "kind" in e &&
    "message" in e &&
    typeof (e as Record<string, unknown>).message === "string"
  );
}

/** A human-readable message for any rejection (CommandError, Error, string, or unknown). */
export function errorMessage(e: unknown): string {
  if (isCommandError(e)) return e.message;
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return "Something went wrong";
}

/** The error kind, when the rejection is a structured CommandError. */
export function errorKind(e: unknown): CommandErrorKind | null {
  return isCommandError(e) ? e.kind : null;
}

// Coarse reason for a failed outgoing message, derived FRONTEND-SIDE from whatever the
// send rejection currently exposes (a CommandError kind, or — until the backend grows
// structured send errors — keyword-sniffing the message string). Keep this mapping the
// single place that knows the error shape, so a later batch can upgrade it in one spot.
export type SendFailReason =
  | "peer-unknown" // the recipient/peer isn't known / not discovered / offline
  | "relay-unreachable" // the node/relay/network couldn't carry the message
  | "crypto" // encryption / key / session failure
  | "unknown";

export function sendFailReason(e: unknown): SendFailReason {
  const msg = errorMessage(e).toLowerCase();
  const kind = errorKind(e);

  if (/crypt|encrypt|decrypt|\bkey\b|ratchet|session|cipher/.test(msg))
    return "crypto";
  if (
    /unknown peer|no peer|peer not|not found|undiscovered|not discovered|no route|recipient|account not|offline/.test(
      msg,
    )
  )
    return "peer-unknown";
  if (
    kind === "network" ||
    /network|unreachable|connect|timed out|timeout|relay|post office|post-office|node down|not ready|transport|send fail/.test(
      msg,
    )
  )
    return "relay-unreachable";
  return "unknown";
}
