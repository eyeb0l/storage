#!/bin/sh
set -eu
root=${0%/*}
[ "$1" = --config ] && [ "$2" = "$root/.docker" ] || exit 90
[ "$3" = --context ] && [ "$4" = desktop-linux ] || exit 91
shift 4
# Assert isolation in the actual child process too. Never forward any command to Docker.
[ "${DOCKER_HOST+x}${DOCKER_CONTEXT+x}${DOCKER_CONFIG+x}${BUILDX_BUILDER+x}${BUILDKIT_HOST+x}" = '' ] || exit 92
printf '%s\n' "$*" >> "$root/calls"
case "$1 $2" in
  'context inspect') cat "$root/context.json" ;;
  'info --format')
    if [ -f "$root/fail-info" ]; then exit 17; fi
    if [ -f "$root/block-info" ]; then exec /bin/sleep 30; fi
    cat "$root/engine.json" ;;
  'container ls')
    if [ -f "$root/change-membership" ]; then
      cat "$root/containers"
      printf '\n%s\n' dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd >> "$root/containers"
      rm "$root/change-membership"
    else cat "$root/containers"; fi ;;
  'image ls') cat "$root/images" ;;
  'container inspect') cat "$root/$4.json" ;;
  'image inspect')
    if [ -f "$root/change-after-inspect" ]; then
      cat "$root/$3.json"
      cp "$root/changed-image.json" "$root/$3.json"
      rm "$root/change-after-inspect"
    else cat "$root/$3.json"; fi ;;
  *) printf 'unexpected command\n' >&2; exit 99 ;;
esac
