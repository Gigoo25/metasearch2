#!/bin/sh
# Starts kiwix-serve next to metasearch so the whole deployment is a single
# container. kiwix-serve listens on loopback only and is reverse-proxied by
# metasearch at /kiwix.
set -eu

ZIM_DIR="${KIWIX_ZIM_DIR:-/zim}"
KIWIX_PORT="${KIWIX_PORT:-8090}"
KIWIX_LIBRARY="${KIWIX_LIBRARY:-/cache/kiwix-library.xml}"
CONFIG="${CONFIG:-/usr/local/bin/config.toml}"

mkdir -p "$(dirname "$KIWIX_LIBRARY")"

# the zim directory is the source of truth, so the library is rebuilt first,
# then kiwix is started; `-M` reloads the library whenever it changes
(
  cp /usr/local/share/kiwix-empty-library.xml "$KIWIX_LIBRARY"
  if [ -d "$ZIM_DIR" ]; then
    find "$ZIM_DIR" -type f -name '*.zim' 2>/dev/null | while IFS= read -r zim; do
      if ! kiwix-manage "$KIWIX_LIBRARY" add "$zim" >/dev/null; then
        echo "metasearch: failed to add $zim to the kiwix library"
      fi
    done
  fi

  while true; do
    kiwix-serve \
      --port="$KIWIX_PORT" \
      --address=127.0.0.1 \
      --urlRootLocation=/kiwix \
      --library "$KIWIX_LIBRARY" \
      -M || true
    echo "metasearch: kiwix-serve exited, restarting in 1s"
    sleep 1
  done
) &

# metasearch starts right away; kiwix comes up in the background and searches
# wait for it internally

# ZIM files can also be added while running, without restarting the container:
#   docker exec <container> kiwix-manage /cache/kiwix-library.xml add /zim/new.zim
# (the library is rebuilt from $ZIM_DIR on the next container start)

exec /usr/local/bin/metasearch "$CONFIG"
