// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// URL-share codec for the Playground (the Rust Playground's killer feature):
// the editor's script is encoded into the URL so a link opens with that exact
// script ready to Run. Dependency-free and browser-native — deflate-raw via
// `CompressionStream` when available (`?s=`), falling back to a plain base64 of
// the UTF-8 bytes (`?u=`). Decode accepts either marker on load.

function toB64url(bytes) {
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function fromB64url(s) {
  s = s.replace(/-/g, "+").replace(/_/g, "/");
  while (s.length % 4) s += "=";
  const bin = atob(s);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes;
}

async function deflateRaw(bytes) {
  const cs = new CompressionStream("deflate-raw");
  const writer = cs.writable.getWriter();
  writer.write(bytes);
  writer.close();
  return new Uint8Array(await new Response(cs.readable).arrayBuffer());
}

async function inflateRaw(bytes) {
  const ds = new DecompressionStream("deflate-raw");
  const writer = ds.writable.getWriter();
  writer.write(bytes);
  writer.close();
  return new Uint8Array(await new Response(ds.readable).arrayBuffer());
}

// Encodes a script into a URL query string ("s=..." deflated, or "u=..." plain).
export async function encodeScript(text) {
  const utf8 = new TextEncoder().encode(text);
  if (typeof CompressionStream !== "undefined") {
    try {
      return "s=" + toB64url(await deflateRaw(utf8));
    } catch (_) {
      /* fall through to the plain encoding */
    }
  }
  return "u=" + toB64url(utf8);
}

// Decodes a script from a location.search string, or null if none/invalid.
export async function decodeScript(search) {
  const query = search && search.startsWith("?") ? search.slice(1) : search || "";
  const params = new URLSearchParams(query);
  if (params.has("s") && typeof DecompressionStream !== "undefined") {
    try {
      return new TextDecoder().decode(await inflateRaw(fromB64url(params.get("s"))));
    } catch (_) {
      return null;
    }
  }
  if (params.has("u")) {
    try {
      return new TextDecoder().decode(fromB64url(params.get("u")));
    } catch (_) {
      return null;
    }
  }
  return null;
}
