# Changelog

All notable changes to this project are listed here ([Keep a Changelog](https://keepachangelog.com), [SemVer](https://semver.org)).

## [Unreleased]

## [0.3.0] - 2026-11-16

- Touch-panel endpoints: `GET /api/health`, `POST /api/privacy`, `POST /api/stream-mic`, `POST /api/clip`
  (new scope `clips:edit`: run `magi-overlay login` again). New `[obs] mic` setting.
- CS2 state includes `bomb` (planted / defused / exploded) from game state integration.

## [0.2.2] - 2026-11-16

- `POST /api/say {"text": …}` sends a chat message as the broadcaster (new scope `user:write:chat`:
  run `magi-overlay login` again).

## [0.2.1] - 2026-11-16

- `POST /api/channel`: change title, category, tags, language and mature label mid-stream (no OBS involved);
  the overlay shows the new title/category immediately instead of after the next 60 s poll.
- `POST /api/scene {"scene": NAME}`: switch OBS scenes from outside OBS.
- Fixed: `POST /api/channel` and `POST /api/presets` returned 405 because their GET routes shadowed the
  generic POST handler.

## [0.2.0] - 2026-11-16

- Go live without touching OBS: `POST /api/golive` sets the Twitch title, category, tags, language and
  mature-language label, picks the opening scene + countdown, starts the stream, then posts a chat
  announcement and runs an optional `[golive] notify_command`. `POST /api/endstream` shows the ending
  scene and stops the stream after `end_delay_secs`.
- `GET /api/channel`, `GET /api/categories?q=` (with box art) and stream presets (`/api/presets`).
- New Twitch scopes `channel:manage:broadcast` and `moderator:manage:announcements`: run `magi-overlay login` again.
- `setup` no longer resets crops/stretch you set by hand on existing camera items.

## [0.1.0] - 2026-11-16

- First release.
