#!/bin/sh
# Not enabled or started: a mock that adds network interfaces is for the
# person installing it to switch on, after reading the blueprint.
systemctl daemon-reload >/dev/null 2>&1 || :
cat <<'MSG'

  breeze-mock installed. Its blueprint is /etc/breeze-mock/blueprint.xml;
  examples are in /usr/share/breeze-mock/examples/.

    breeze-mock check /etc/breeze-mock/blueprint.xml
    systemctl enable --now breeze-mock

MSG
