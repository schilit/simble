// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// "What this page is doing" — the one box every domain opens with.
//
// Descriptions had grown unevenly: Generic carried an intro paragraph,
// Ranging a whole panel headed "What this page is doing", and the other five
// domains explained themselves nowhere. A reader arriving at a tab could not
// rely on being told what they were looking at.
//
// It is a <details>, so the explanation is there the first time and foldable
// afterwards — the pages are demonstrations, and a paragraph you have already
// read is just chrome above the thing you came to see. The open/closed choice
// is remembered across domains, because it is a preference about reading
// rather than about any one page.

const STORE_KEY = "simble-about-open";

/// Builds the box. `html` is page-authored prose — links and <code> are
/// expected — never anything a device reported.
export function createAboutBox(html) {
  const box = document.createElement("details");
  box.className = "about full";

  // Default open: a first-time reader should not have to discover the
  // explanation. localStorage can throw outright in a private window or with
  // site data blocked, so a failure to read it just means "open".
  let open = true;
  try {
    open = localStorage.getItem(STORE_KEY) !== "closed";
  } catch (e) {
    /* no stored preference is a fine answer */
  }
  box.open = open;

  const summary = document.createElement("summary");
  summary.textContent = "What this page is doing";
  const body = document.createElement("div");
  body.className = "about-body";
  body.innerHTML = html;
  box.append(summary, body);

  box.addEventListener("toggle", () => {
    try {
      localStorage.setItem(STORE_KEY, box.open ? "open" : "closed");
    } catch (e) {
      /* the box still works, it just will not be remembered */
    }
  });
  return box;
}
