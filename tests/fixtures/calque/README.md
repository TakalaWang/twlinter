# EN→ZH calque fixtures

Paired Before/After examples driving the `calque_fixtures` integration
suite.  Each fixture has a stable ID embedded in its filename so
detector gates can name the exact fixtures they cover.

## Provenance

Diagnostic patterns and the six-red-flag taxonomy are distilled from a
third-party MIT-licensed EN→ZH translation review checklist (see commit
history for the upstream attribution).  No raw text from the upstream
project's Simplified Chinese excerpts appears verbatim — every fixture
has been:

1. Converted to Traditional Chinese suitable for zh-TW input
2. Paraphrased away from any third-party copyrighted source quoted in
   the upstream references.  The diagnostic Before/After structure is
   preserved; specific wording is substituted with neutral text.

## Stable ID scheme

```
calque_<category>_<detector>_<bad|good|solo>_<NNN>.txt
```

Where:

* `<category>` is one of:
  * `superlative` — `最…之一` calque (Red Flag 4)
  * `connective` — bounded EN connective calques (Red Flag 2)
  * `nominalization` — verb-noun calque chains (Red Flag 6)
  * `premodifier` — long pre-modifier 定語堆疊 (Red Flag 3)
  * `falsefriend` — literal-mistranslation lexical pairs
* `<detector>` is the detector key (`zy1`, `zy1b`, `zy2`, `zy2b`,
  `zy3`, `zy3b`, `zy4`, `zy5`).
* `bad` should fire the detector; `good` should not; `solo` is a
  same-pattern occurrence in isolation that ZY4a's same-span guard
  must also pass.

## Detector coverage gates

Substring-only detectors:

* ZY1a (`zy1`): `bad_*` ≥ 3 examples; `good_*` includes biographical
  idiom (`當代最傑出的畫家之一`) which is suppressed via the
  person-class noun guard.
* ZY2a (`zy2`): `bad_*` covers all four patterns
  (因為…所以 / 雖然…但是 / 當…的時候 / 如果…那麼).
* ZY3a (`zy3`): `bad_*` covers the three nominalization-pair shapes.
* ZY4a (`zy4`): `bad_*` ≥ 5 false-friend pairs; `solo_*` verifies the
  same-span guard suppresses standalone occurrences.

Boundary-aware detectors:

* ZY1b (`zy1b`): `bad_*` paragraph with high `之一` density triggers
  the per-domain density check; `good_*` keeps the calque structure
  but drops occurrences below the threshold.
* ZY2b (`zy2b`): `bad_*` keeps the connective pair inside one
  sentence; `good_*` separates the pair across `。` boundaries —
  ZY2b must not fire (ZY2a may still fire on the same `good_*` since
  it ignores sentence boundaries; the harness checks only the named
  detector for boundary-aware good fixtures).
* ZY3b (`zy3b`): `bad_*` chains three abstract heads
  (`改善的提升的發現`); `good_*` shortens to two heads.

Long pre-modifier detector:

* ZY5 (`zy5`): `bad_*` is the long-pre-modifier archetype
  (`那個在車站外面的雨裡等了三個小時的男人終於放棄了`).
  `good_*` covers two negative cases — native long names like
  `中華民國行政院` (no internal `的`) and the same calque text with
  internal commas restoring native breath-group rhythm.

Substring-only `_good_*` and `_solo_*` fixtures emit zero ZY issues
across ALL detectors.  Boundary-aware `_good_*` fixtures emit zero
issues for their NAMED detector only — looser-window detectors may
still fire.
