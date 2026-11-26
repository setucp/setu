import { ApiError } from "./client";

/**
 * An expired session surfaces as a 401: send the visitor back to sign in
 * once, with the way back. Returns whether it redirected.
 */
export function redirectOnUnauthorised(error: unknown): boolean {
  if (!(error instanceof ApiError) || error.status !== 401) return false;
  const back = window.location.pathname;
  if (back === "/login" || back === "/setup") return false;
  window.location.assign(`/login?redirect=${encodeURIComponent(back)}`);
  return true;
}
