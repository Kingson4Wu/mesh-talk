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
