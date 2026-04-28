# Mobile frontend — manual verification

Run on a real iOS device (iPhone with Safari) before merging the
mobile-friendly branch. Headless Chromium does not reproduce these
quirks.

## Checklist

- [ ] Open https://<staging-url>/#dusk-collective in Mobile Safari.
- [ ] Tap the composer input — the page must NOT zoom in.
- [ ] Type a long draft until the keyboard is clearly up — the
      composer stays visible above the keyboard, not hidden behind it.
- [ ] Open the rooms drawer; tap the search input — no zoom.
- [ ] Scroll the chat view — no rubber-band on the address bar; the
      sticky header stays at the top.
- [ ] Rotate to landscape — header + composer reflow without horizontal
      scrollbar.
- [ ] Notch / dynamic island devices: header content is below the
      notch, not under it.
- [ ] Home-indicator devices: composer + voice mini-bar sit above the
      indicator, not under it.
- [ ] Long-press a room in the rooms drawer; drag onto a different
      row; release — order updates.
- [ ] Tap an in-call peer in the channels drawer — voice sheet opens.
- [ ] Tap the message info button — details sheet opens; backdrop tap
      dismisses.
- [ ] Tap the voice mini-bar — opens the user's own voice sheet.
- [ ] Switch theme via the rooms-drawer footer — persists across
      reload.

If any item fails, file the issue with a video / screenshot and link
back to this branch's PR.
