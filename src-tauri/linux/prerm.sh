#!/bin/sh
# Stops the service (it gives the PC its own routing back) before its file
# goes. An upgrade keeps it enabled; the new package restarts it.
set -e
if [ -d /run/systemd/system ]; then
    case "$1" in
        remove|deconfigure)
            systemctl disable --now netagg.service || true
            # Every user's tray app (the service has stopped, so any netagg left is one).
            pkill -x netagg || true
            ;;
        upgrade) systemctl stop netagg.service || true ;;
    esac
fi
