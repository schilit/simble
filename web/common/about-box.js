// Copyright 2026 Bill Schilit — SPDX-License-Identifier: Apache-2.0
//
// "About this page" — the one box every domain opens with.
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
///
/// `key` gives a page its own remembered preference instead of the shared one.
/// The shared key is right for the seven domain tabs, which are interchangeable
/// views of the same shell — collapsing the box on one and finding it collapsed
/// on the next is the point. A standalone workspace page is not one of those:
/// Scene is somewhere you arrive needing orientation, so it keeps its own
/// preference and starts open even for a reader who folded the domains' box
/// away weeks ago.
export function createAboutBox(html, { key = "" } = {}) {
  const box = document.createElement("details");
  box.className = "about full";
  const storeKey = key ? `${STORE_KEY}:${key}` : STORE_KEY;

  // Default open: a first-time reader should not have to discover the
  // explanation. localStorage can throw outright in a private window or with
  // site data blocked, so a failure to read it just means "open".
  let open = true;
  try {
    open = localStorage.getItem(storeKey) !== "closed";
  } catch (e) {
    /* no stored preference is a fine answer */
  }
  box.open = open;

  const summary = document.createElement("summary");
  summary.textContent = "About this page";
  const body = document.createElement("div");
  body.className = "about-body";
  body.innerHTML = html;
  box.append(summary, body);

  box.addEventListener("toggle", () => {
    try {
      localStorage.setItem(storeKey, box.open ? "open" : "closed");
    } catch (e) {
      /* the box still works, it just will not be remembered */
    }
  });
  return box;
}
