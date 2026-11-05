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
# Where earendil speaks: an ALSA playback device (the TV over HDMI).
AUDIO_OUT=${AUDIO_OUT:-default}
# The office (public): Hiroshima, its JMA forecast office / area and the
# AMeDAS point for temperatures (67437 = 広島).
ATLAS_LAT=${ATLAS_LAT:-34.4080}
ATLAS_LON=${ATLAS_LON:-132.4547}
ATLAS_FORECAST_OFFICE=${ATLAS_FORECAST_OFFICE:-340000}
ATLAS_FORECAST_AREA=${ATLAS_FORECAST_AREA:-340010}
ATLAS_TEMP_POINT=${ATLAS_TEMP_POINT:-67437}
TARGET="$DISPLAY_USER@$DISPLAY_HOST"
SSH="ssh ${SSH_OPTS:-} $TARGET"

# These end up in a unit file through sed; only plain numbers are accepted.
for value in "$ATLAS_LAT" "$ATLAS_LON" "$ATLAS_FORECAST_OFFICE" "$ATLAS_FORECAST_AREA" "$ATLAS_TEMP_POINT"; do
    case "$value" in
        "" | *[!0-9.-]*) echo "invalid atlas setting: $value" >&2; exit 1 ;;
    esac
done
case "$AUDIO_OUT" in
    "" | *[!A-Za-z0-9:,=_.-]*) echo "invalid AUDIO_OUT: $AUDIO_OUT" >&2; exit 1 ;;
esac

render() {
    sed -e "s|@USER@|$DISPLAY_USER|g" \
        -e "s|@HOME@|$HOME_DIR|g" \
        -e "s|@APP_ROOT@|$APP_ROOT|g" \
        -e "s|@PYTHON@|$PYTHON|g" \
        -e "s|@AUDIO_DEVICE@|$AUDIO_DEVICE|g" \
        -e "s|@AUDIO_OUT@|$AUDIO_OUT|g" \
        -e "s|@ATLAS_LAT@|$ATLAS_LAT|g" \
        -e "s|@ATLAS_LON@|$ATLAS_LON|g" \
        -e "s|@ATLAS_FORECAST_OFFICE@|$ATLAS_FORECAST_OFFICE|g" \
        -e "s|@ATLAS_FORECAST_AREA@|$ATLAS_FORECAST_AREA|g" \
        -e "s|@ATLAS_TEMP_POINT@|$ATLAS_TEMP_POINT|g" "$1"
}

# presence-dimmer (plain python, no dependencies)
$SSH "mkdir -p $HOME_DIR/presence-dimmer"
tar cf - -C presence . | $SSH "tar xf - -C $HOME_DIR/presence-dimmer"

# room-survey (analysis + drawing service)
$SSH "mkdir -p $HOME_DIR/room-survey"
tar cf - -C survey . | $SSH "tar xf - -C $HOME_DIR/room-survey"

# speech (owns the microphone; rough captions)
$SSH "mkdir -p $HOME_DIR/speech"
tar cf - --exclude=./tests --exclude=__pycache__ --exclude=.pytest_cache --exclude=./.venv --exclude=./.venv-whisper --exclude=./.whisper-diagnostic --exclude=./speech.env -C speech . | $SSH "tar xf - -C $HOME_DIR/speech"

# office-stats (minute aggregates, SQLite)
$SSH "mkdir -p $HOME_DIR/office-stats"
tar cf - -C stats . | $SSH "tar xf - -C $HOME_DIR/office-stats"

# atlas (the only service that talks to the internet; numpy + Pillow)
$SSH "mkdir -p $HOME_DIR/atlas"
tar cf - --exclude=./tests --exclude=__pycache__ --exclude=.pytest_cache -C atlas . | $SSH "tar xf - -C $HOME_DIR/atlas"

# earendil (values band + voice agent; talks to the DeepSeek API and earendil.com).
# Its virtualenv holds the OpenAI-compatible SDK. The API key lives in earendil.env on
# the display (mode 0600), which this script never touches.
$SSH "mkdir -p $HOME_DIR/earendil"
tar cf - --exclude=./tests --exclude=__pycache__ --exclude=.pytest_cache --exclude=./.venv --exclude=./earendil.env -C earendil . | $SSH "tar xf - -C $HOME_DIR/earendil"
$SSH "cd $HOME_DIR/earendil && { [ -x .venv/bin/python ] || $PYTHON -m venv .venv; } && \
      .venv/bin/pip install -q --upgrade -r requirements.txt"

# systemd units
for unit in photo-frame-rs.service presence-dimmer.service room-survey.service office-stats.service speech.service atlas.service earendil.service; do
    echo "installing $unit"
    render "systemd/$unit" | $SSH "sudo tee /etc/systemd/system/$unit >/dev/null"
done

# X session: only X + window manager; the app itself is a systemd service
render display-session/xinitrc | $SSH "cat > $HOME_DIR/.xinitrc && chmod +x $HOME_DIR/.xinitrc"
render display-session/bash_profile | $SSH "cat > $HOME_DIR/.bash_profile"

$SSH "sudo systemctl daemon-reload && \
      sudo systemctl enable photo-frame-rs.service presence-dimmer.service room-survey.service office-stats.service speech.service atlas.service earendil.service"

# atlas and earendil have no build step: the code copied above only runs after
# a (re)start. enable --now starts them on a first install, try-restart picks
# up new code. speech is restarted for the phrase feed earendil reads.
$SSH "sudo systemctl enable --now atlas.service earendil.service && \
      sudo systemctl try-restart atlas.service earendil.service speech.service"

echo "installed. atlas, earendil and speech restarted; X session changes apply on the next reboot."
echo "earendil answers only once $HOME_DIR/earendil/earendil.env holds DEEPSEEK_API_KEY (chmod 600)."
echo "The photo frame starts from $APP_ROOT/current/bin — run build-and-deploy.sh first."
