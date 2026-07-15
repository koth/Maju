// Account login: obtains an `auth_token` used for `BindDeviceRequest`. The
// exact relay login contract (in-app login screen vs. token exchange) is TBD
// pending the relay-service spec; this module defines the interface the app
// wires to. The phone never owns subscription/payment logic — it only holds
// the token and surfaces `SubscriptionStatus`.

export interface LoginCredentials {
  account: string;
  token: string;
}

export interface LoginProvider {
  /** Exchange credentials for a relay `auth_token`. */
  login(credentials: LoginCredentials): Promise<string>;
}

/**
 * Build a `BindDeviceRequest` envelope payload from a freshly-obtained token.
 * The caller sends it over the relay and maps the response with
 * `bindOutcomeFromResponse`.
 */
export function bindDeviceRequest(authToken: string): { auth_token: string } {
  return { auth_token: authToken };
}

/** A no-op login provider for the free tier (no account). Free-tier pairing
 * requires no login; this exists so the account module has a typed default. */
export const anonymousLoginProvider: LoginProvider = {
  async login() {
  throw new Error("anonymous: no login required for free-tier pairing");
  },
};
// end of file
