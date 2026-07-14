#!/usr/bin/env sh
set -eu
root="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
mkdir -p "$root/flac" "$root/mp3" "$root/m4a" "$root/ogg" "$root/missing" "$root/conflict"
base="-f lavfi -i anullsrc=r=44100:cl=mono -t 0.1 -metadata artist=日本語アーティスト -metadata album=テストアルバム -metadata title=楽曲"
ffmpeg -y $base "$root/flac/japanese.flac"
ffmpeg -y $base "$root/mp3/japanese.mp3"
ffmpeg -y $base "$root/m4a/japanese.m4a"
ffmpeg -y $base "$root/ogg/japanese.ogg"
ffmpeg -y -f lavfi -i anullsrc=r=44100:cl=mono -t 0.1 -metadata album=テストアルバム "$root/missing/no-artist.mp3"
ffmpeg -y $base "$root/conflict/one.mp3"
ffmpeg -y $base "$root/conflict/two.mp3"
