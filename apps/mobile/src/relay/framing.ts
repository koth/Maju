import { PROTO_VERSION, type Envelope, type Message } from "../types/relay-protocol";

// Envelope <-> Message translation, mirroring relay-protocol::envelope.
// The wire `Envelope` is `{ proto_version, id?, type, payload }`. A `Message`
// is adjacently tagged `{ type, payload }`; `fromMessage` splits that into the
// envelope's `type` discriminator and `payload` body. Unknown `type` values
// map to the `Message` catch-all so newer peers do not break older ones.

/** Build an envelope from a typed message. `id` is the request/response id
 * (null for unsolicited events). */
export function fromMessage(id: string | null, message: Message): Envelope {
  return {
    proto_version: PROTO_VERSION,
    id: id ?? null,
    type: message.type,
    payload: message.payload,
  };
}

/** Interpret an envelope as a typed message. Unknown discriminators map to
 * the catch-all `{ type: env.type, payload: env.payload }`. No payload
 * validation is performed (both ends share the contract); callers narrow by
 * `type` and cast `payload`. */
export function intoMessage(env: Envelope): Message {
  return { type: env.type, payload: env.payload };
}

/** Serialize an envelope to a JSON text frame. */
export function serializeEnvelope(env: Envelope): string {
  return JSON.stringify(env);
}

/** Parse a JSON text frame into an envelope. */
export function parseEnvelope(text: string): Envelope {
  return JSON.parse(text) as Envelope;
}
