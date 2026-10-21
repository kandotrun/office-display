#!/bin/sh
# Install the display application, systemd units and X session files on the
# display machine.
#
#   DISPLAY_HOST=... DISPLAY_USER=... ./deploy/install-units.sh
#
# Everything machine-specific is injected from the environment here so that no
# host name, user name or address lives in the repository.
set -eu

: "${DISPLAY_HOST:?set DISPLAY_HOST to the display machine}"
: "${DISPLAY_USER:?set DISPLAY_USER to the display account}"

HOME_DIR=${HOME_DIR:-/home/$DISPLAY_USER}
APP_ROOT=${APP_ROOT:-$HOME_DIR/photo-frame-rs}
PYTHON=${PYTHON:-python3}
AUDIO_DEVICE=${AUDIO_DEVICE:-plughw:0,0}
TARGET="$DISPLAY_USER@$DISPLAY_HOST"
SSH="ssh ${SSH_OPTS:-} $TARGET"

render() {
    sed -e "s|@USER@|$DISPLAY_USER|g" \
        -e "s|@HOME@|$HOME_DIR|g" \
        -e "s|@APP_ROOT@|$APP_ROOT|g" \
        -e "s|@PYTHON@|$PYTHON|g" \
        -e "s|@AUDIO_DEVICE@|$AUDIO_DEVICE|g" "$1"
}

# presence-dimmer (plain python, no dependencies)
$SSH "mkdir -p $HOME_DIR/presence-dimmer"
tar cf - -C presence . | $SSH "tar xf - -C $HOME_DIR/presence-dimmer"

# room-survey (analysis + drawing service)
$SSH "mkdir -p $HOME_DIR/room-survey"
tar cf - -C survey . | $SSH "tar xf - -C $HOME_DIR/room-survey"

# systemd units
for unit in photo-frame-rs.service presence-dimmer.service room-survey.service; do
    echo "installing $unit"
    render "systemd/$unit" | $SSH "sudo tee /etc/systemd/system/$unit >/dev/null"
done

# X session: only X + window manager; the app itself is a systemd service
render display-session/xinitrc | $SSH "cat > $HOME_DIR/.xinitrc && chmod +x $HOME_DIR/.xinitrc"
render display-session/bash_profile | $SSH "cat > $HOME_DIR/.bash_profile"

$SSH "sudo systemctl daemon-reload && \
      sudo systemctl enable photo-frame-rs.service presence-dimmer.service room-survey.service"

echo "installed. X session changes apply on the next reboot."
echo "The photo frame starts from $APP_ROOT/current/bin — run build-and-deploy.sh first."
