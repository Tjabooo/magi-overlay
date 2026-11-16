# Changelog

All notable changes to this project are listed here ([Keep a Changelog](https://keepachangelog.com), [SemVer](https://semver.org)).

## [Unreleased]

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
