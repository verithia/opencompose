import { invoke } from "@tauri-apps/api/core";

export type OAuthState = "connected" | "expired" | "needs_reauth" | "missing";
/** `cancelled` is the caller giving up, which is not a failure. */
export type OAuthAttemptState = "pending" | "complete" | "cancelled" | "failed" | "expired";

export interface OAuthStatus {
  state: OAuthState;
  account: string | null;
  expires_at: number | null;
  reauth_required: boolean;
  message: string;
}

/** A browser sign-in that returns to a loopback listener. */
export type OAuthMode = "auto";

export interface OAuthStart {
  attempt_id: string;
  url: string;
  mode: OAuthMode;
  expires_at: number;
  opened: boolean;
}

/** Why a sign-in attempt ended without a credential. The panel keys on `kind`,
 * never on the message text. */
export type SignInErrorKind = "expired" | "failed";

export interface SignInError {
  kind: SignInErrorKind;
  message: string;
}

export interface OAuthAttempt {
  state: OAuthAttemptState;
  message: string;
  /** Present on a failed or expired attempt. */
  error?: SignInError;
}

export function oauthStatus(providerId: string): Promise<OAuthStatus> {
  return invoke<OAuthStatus>("ai_oauth_status", { providerId });
}

export function oauthBegin(providerId: string): Promise<OAuthStart> {
  return invoke<OAuthStart>("ai_oauth_begin", { providerId });
}

export function oauthPoll(attemptId: string): Promise<OAuthAttempt> {
  return invoke<OAuthAttempt>("ai_oauth_poll", { attemptId });
}

export function oauthCancel(attemptId: string): Promise<void> {
  return invoke<void>("ai_oauth_cancel", { attemptId });
}

export function oauthSignOut(providerId: string): Promise<void> {
  return invoke<void>("ai_oauth_sign_out", { providerId });
}
