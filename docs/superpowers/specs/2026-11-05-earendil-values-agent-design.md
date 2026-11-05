# Earendil: the values band and the voice consultation agent

Status: approved design, 2026-11-05

## Goal

The team was taken by Earendil Inc.'s published values
(<https://earendil.com/values/>) and wants to use them as a compass, in the
spirit of "ヒンメルならそうした": when unsure, ask what Earendil would do.
The team calls it **エンデルヒール**.

- The values stay on screen at all times, one at a time, next to the
  Earendil emblem.
- Anyone can talk to the TV: 「エンデルヒールなら、締め切り前の徹夜ってどう思う？」.
  The emblem comes forward, "speaks" (glows and pulses with its voice) and
  answers in two to four spoken sentences grounded in the values.

## Decisions

| Topic | Decision |
|---|---|
| Placement | A permanent glass **band** under the photo collage (right column). A conversation expands the band over the collage, then collapses back. The left strip is unchanged. |
| Service | New loopback service `earendil` (`127.0.0.1:8101`, Python). It listens to `speech`, detects the wake phrase, asks DeepSeek, speaks the answer and publishes state. `photo-frame-rs` only reads it. |
| Wake | Fuzzy kana matching on the rough Vosk captions; no second recogniser. Measured: エンデルヒール → 「エンゼル ヒール」 consistently. |
| LLM | DeepSeek API via the `openai` Python SDK at `https://api.deepseek.com`, model `deepseek-flash`, reasoning effort `none`, streamed. Configurable through the environment; credentials are read only from `DEEPSEEK_API_KEY`. |
| Voice | Pluggable TTS: VOICEVOX engine (HTTP), Open JTalk (CLI), macOS `say` (dev) or none (subtitles only). Playback through `aplay`/`afplay`. |
| Emblem | Not stored in the repository. Fetched at runtime from `EARENDIL_EMBLEM_URL` (default `https://earendil.com/static/earendil-emblem.svg`, or a local path), flattened to polygons by the service and filled by the frame. |
| Values text | `earendil/values.json`: English titles plus our own Japanese summaries. The original page text is fetched at runtime (daily, cached) and given to the model as reference; it is never committed. |
| Privacy | Audio never leaves the machine. Only the transcribed text of an utterance that contains the wake phrase is sent to the DeepSeek API. Questions and answers are kept in memory only (last few exchanges, a few minutes) and never logged or written to disk. |

## Data flow

```
 mic ─▶ speech (8098) ──GET /status (phrases, partial)──▶ earendil (8101) ──▶ DeepSeek API
                                                            │   ▲
                                                            │   └─ VOICEVOX (50021) / open_jtalk / say
                                                            ├─▶ aplay / afplay ─▶ HDMI speakers
                                                            ▼
                              photo-frame-rs ◀── GET /status, /values.json, /emblem.json
```

## Contracts

All JSON is UTF-8. Unknown fields must be ignored by readers. Times are unix
seconds (float) from the same host clock.

### speech `GET /status` (additive change)

Existing fields stay. New field:

```json
"phrases": [{"seq": 41, "at": 1793782800.12, "text": "エンゼル ヒール なら どう 思う"}]
```

- The last `SPEECH_MAX_PHRASES` (default 6) final phrases, oldest first,
  independent of the caption hold.
- `seq` increases by one per final phrase and restarts at 1 when the service
  restarts. A reader that sees `seq` go backwards resets its cursor.
- `at` is the end time of captured speech. Local Whisper also supplies
  `started_at` for the beginning of its captured interval, including pre-roll;
  neither timestamp is replaced with the later CPU decoding completion time.

### earendil `GET /values.json`

```json
{
  "source": "https://earendil.com/values/",
  "values": [
    {"title": "We not I", "kind": "value", "ja": "「私」ではなく「私たち」",
     "gloss": "最高のチームは個の総和を超える。ひとりでは謙虚に、チームとしては自信をもって。"}
  ]
}
```

- `kind` is `"value"` or `"principle"`. Order is the page order.
- At most 32 entries; `title` ≤ 64 chars, `ja` ≤ 40, `gloss` ≤ 120.

### earendil `GET /emblem.json`

```json
{"aspect": 0.549, "rings": [[[0.98, 0.67], [0.97, 0.80], ...]]}
```

- `aspect` = width / height of the emblem's view box, 0.1..10.
- `rings`: closed polygons in view-box fractions, `x` and `y` in 0..1, `y`
  down. Fill with the even-odd rule. 1..64 rings, 3..4096 points in total.
- 404 until an emblem has been loaded (fetch failed and no cache); the frame
  then draws its own fallback star.

### earendil `GET /status`

```json
{
  "state": "speaking",
  "conversation": 12,
  "persona": "エンデルヒール",
  "hint": "「エンデルヒールなら？」と話しかけてみてください",
  "question": "締め切り前に徹夜するのってどう思う",
  "heard": "",
  "answer": "エンデルヒールなら、まず徹夜しなくて済む方法をチームで探すよ。…",
  "values": ["We not I", "Family First"],
  "voice": {"started": 1793782803.5, "step": 0.05, "levels": [0.0, 0.42, 0.81]},
  "voice_credit": "VOICEVOX:青山龍星",
  "error": null
}
```

| Field | Meaning |
|---|---|
| `state` | `idle` · `listening` (wake heard, waiting for the question) · `thinking` (asked the model, nothing to show yet) · `speaking` (answer streaming and/or voice playing) · `answered` (done; held on screen) · `error` (could not answer; held like `answered`) |
| `conversation` | Increments on every wake. The frame restarts its conversation animation when it changes. 0 before the first one. |
| `question` | Transcribed question without the wake phrase. `""` when idle. ≤ 200 chars. |
| `heard` | Live partial transcript while `listening`; `""` otherwise. ≤ 200 chars. |
| `answer` | The answer text so far (grows while streaming), without the value tag line. ≤ 400 chars. |
| `values` | Titles from `/values.json` the answer is grounded in (0..3). |
| `voice` | The sentence being spoken right now: start time, sample step in seconds and loudness levels 0..1 (≤ 1200). `null` when not speaking. |
| `voice_credit` | Credit line the TTS engine requires (VOICEVOX), else `""`. |
| `error` | `null` or one of `no_api_key`, `llm_failed`, `llm_refused`, `not_understood` (the frame shows a Japanese message per code). |

The service holds `answered`/`error` for `EARENDIL_HOLD_SECONDS` (25) after
the voice ends, then returns to `idle` (with `question`/`answer` cleared).

### earendil `POST /ask` (loopback, development)

`{"text": "締め切り前に徹夜するのってどう思う"}` starts a conversation as if the
wake phrase plus this question had been heard. Returns `202` with
`{"conversation": n}`, or `409` while a conversation is thinking/speaking.
`400` for a bad body, `415` unless `application/json`, `403` unless the Host
is 127.0.0.1/localhost and there is no `Origin` (no browser page can start a
conversation). For `just dev-ask` and tests; the frame never calls it.

## Wake detection

Captions are rough. Measured with `vosk-model-small-ja-0.22` on synthesised
speech:

| Said | Vosk caption |
|---|---|
| エンデルヒールならどう思う？ | エンゼル ヒール なら どう 思う |
| エンデルヒールなら、締め切り前に徹夜するのってどう思う？ | エンゼル ヒール なら 締切 前 に 徹夜 する のっ て どう 思う |
| エンデルヒールだったらどうする？ | エンゼル ビール だっ たら どう する |
| エンデルヒール的にはこの企画ありかな | エンゼル ヒール 的 に は この 企画 あり 金 |
| エンデルヒールさん、相談があるんだけど | エネルギー いる さん 相談 が ある ん だ けど |
| ねえエンデルヒール | 解 連 ベルギー ビール / で れる ビール |
| エアレンディルならどうする？ | えー アレンジ いる なら どう する |
| エアレンディルなら、新しい技術を導入するべきかな | えー で いる なら 新しい 技術 を 導入 する べき か な |
| エアレンディルならどう言うかな | えー で いる なら どう いう か だ / えー られん で いる なら どう いう か な / やれ で いる なら どう いう か から |
| 今日のお昼ご飯どうする？ | 様 の お 昼ごはん どう する (must not wake) |
| それならどう思う？ | それ なら どう 思う (must not wake) |

Rules:

1. Normalise: NFKC, katakana → hiragana, drop whitespace and punctuation.
2. **Strong aliases** wake on their own when the best substring similarity
   (1 − edit distance / alias length) is ≥ 0.8: えんでるひーる, えんぜるひーる,
   えんぜるびーる, えあれんでぃる, えあれんでる.
3. **Weak aliases** (observed mis-hearings that are also ordinary words) wake
   only on an exact match followed by a cue within 4 characters (one edit
   away from the short ones is everyday talk, e.g. 「メンバー で いる なら」): えねるぎーいる,
   えーでいる, えーられんでいる, やれでいる, えーあれんじいる, でれるびーる,
   べるぎーびーる. Cues: なら, だったら, たら, さん, てきに, 的に, って, は.
4. The question is the caption after the matched name, with a leading cue
   (なら, だったら, さん, 的には, 、) removed. If fewer than 3 characters remain,
   enter `listening` and take the next phrase within `EARENDIL_LISTEN_SECONDS`
   (8) as the question; otherwise return to `idle` silently.
5. Echo: captured intervals overlapping the service's speech, or the 2 s after,
   are ignored (the microphone hears the TV), even when decoding finishes later.
   While `thinking`/`speaking`,
   new wakes are ignored. In `answered`/`error`, a wake starts a new
   conversation.

## The model

- `client.chat.completions.create(model="deepseek-flash", max_tokens=4000,
  reasoning_effort="none", stream=True, messages=[system, ...history, question])`.
  The client uses `https://api.deepseek.com`; only streamed `delta.content` is
  displayed and spoken. Any `reasoning_content` is ignored.
- System prompt: `earendil/persona.md` rendered with the persona name, the
  bundled values and (when fetched) the original page text.
- History: the last 3 exchanges, if the previous one ended less than 180 s ago;
  older ones are dropped from memory 180 s after the last answer.
- Output: line 1 is `[Title, Title]` (or `[]`), the rest is the spoken answer.
  Unknown titles are dropped. Answers are cut to 400 chars.
- A refusal delta or `finish_reason == "content_filter"` → `error: llm_refused`. API errors →
  `llm_failed`. No key → `no_api_key` (the band keeps working).
- Sentences (split on 。！？!? and newlines) are handed to the TTS as soon as
  they are complete, so speech starts before the stream ends. Text reaches
  `open_jtalk`/`say` on stdin; only the WAV touches disk, in the runtime
  directory, while it plays. The speech and VOICEVOX URLs must be loopback
  (VOICEVOX receives the sentence in its URL; its access log must be
  discarded by whoever runs it).
- An interrupted stream without a final `finish_reason == "stop"` is an
  error; its unfinished tail is not spoken.

## Screen

```
┌ strip (unchanged) ──────────┐ ┌ collage ─────────────────────────────┐
│ clock                        │ │   floating prints                    │
│ weather                      │ │                                      │
│ attack map                   │ │                                      │
│                              │ │                                      │
│ survey                       │ ├──────────────────────────────────────┤
│                              │ │ ✦  EARENDIL · VALUES  03 / 14        │
│ footer                       │ │ ▲  We not I                          │
│                              │ │ ▼  「私」ではなく「私たち」— 最高の…  │
└──────────────────────────────┘ └────────────── 「エンデルヒールなら？」┘
```

Band (idle):

- Height `clamp(collage.h × 0.22, 130, 260)`, under the collage, same gap as
  elsewhere; the collage area shrinks by band height + gap. Frosted glass like
  the strip (`strip_paint::glass`).
- Left: the emblem, white, with a soft halo that breathes (≈ 6 s period) and a
  four-point sparkle that twinkles at its tip.
- Right of it: a small letter-spaced label `EARENDIL · VALUES 03 / 14` (or
  `OPERATING PRINCIPLES`), the English title large in the thin Latin face used
  by the clock, then the Japanese title and gloss (wrapped, at most 2 lines).
- Bottom right, dim: the `hint` from `/status`.
- Rotates every 18 s: the old value slides up and fades, the new one rises in.
  After a conversation, the band first shows the first value the answer cited.

Conversation (any state other than `idle`):

- The band expands over the whole right column (collage + gap + band) in
  0.7 s (ease-out); the photos behind get a veil so the text reads. It
  collapses back in 0.7 s when the state returns to `idle`.
- Top left: the emblem, large (≈ 30% of the panel height). Speaking: its
  scale follows the voice level (1.0..1.08), the halo brightens, and rings
  ripple outwards on loud syllables. Thinking: a light sweep runs over it.
  Listening: a slow pulse.
- Next to it: the persona name and a state chip
  (聞いています / 考えています / 話しています / こたえ), and the question in
  「」, dim, at most 2 lines.
- Below, full width: the answer, as large as fits (36 pt down to 22 pt at
  1080p), wrapped; while
  listening, the `heard` text dimmed with animated dots instead.
- Bottom: chips with the cited value titles; a small dim footer
  `Earendil の Values をもとに AI が答えています` plus `voice_credit`.
- `error` codes are shown as friendly Japanese lines (APIキーが設定されていません,
  いまは答えられません, うまく聞き取れませんでした …).

If the service is unreachable the band shows the emblem fallback and
"エンデルヒールを準備しています". `--no-earendil` removes the band and gives
the space back to the collage.
