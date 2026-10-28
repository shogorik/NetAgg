#!/bin/sh
# Purging also removes the service's settings, usage and log, and the
# WebUI's password, certificate and user.
set -e
if [ "$1" = purge ]; then
    rm -rf /var/lib/netagg /var/lib/netagg-web /etc/netagg
    if getent passwd netagg-web >/dev/null; then
        deluser --system netagg-web >/dev/null || true
    fi
fi
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload || true
fi
