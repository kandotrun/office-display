#!/bin/sh
# Build photo-frame-rs on the display machine and deploy it as a new release.
#
#   DISPLAY_HOST=... DISPLAY_USER=... ./deploy/build-and-deploy.sh
#
# The build runs on the display machine because that is where the GPU/X
# libraries and the Rust toolchain live.
set -eu

: "${DISPLAY_HOST:?set DISPLAY_HOST to the display machine}"
: "${DISPLAY_USER:?set DISPLAY_USER to the display account}"

HOME_DIR=${HOME_DIR:-/home/$DISPLAY_USER}
APP_ROOT=${APP_ROOT:-$HOME_DIR/photo-frame-rs}
TARGET="$DISPLAY_USER@$DISPLAY_HOST"
SSH="ssh ${SSH_OPTS:-} $TARGET"

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' photo-frame-rs/Cargo.toml | head -1)
[ -n "$VERSION" ] || { echo "cannot read version from Cargo.toml" >&2; exit 1; }
RELEASE="$APP_ROOT/releases/$VERSION-$(date +%Y%m%d-%H%M%S)"

echo "==> syncing source to $TARGET:$APP_ROOT/src"
$SSH "mkdir -p $APP_ROOT/src"
tar czf - -C photo-frame-rs Cargo.toml Cargo.lock src 2>/dev/null \
    | $SSH "tar xzf - -C $APP_ROOT/src"

echo "==> building (version $VERSION)"
$SSH "export PATH=\$HOME/.cargo/bin:\$PATH; cd $APP_ROOT/src && cargo build --release --locked"

echo "==> publishing $RELEASE"
$SSH "set -eu
    mkdir -p '$RELEASE/bin'
    cp $APP_ROOT/src/target/release/photo-frame-rs '$RELEASE/bin/'
    ln -sfn '$RELEASE' $APP_ROOT/current
    sudo systemctl restart photo-frame-rs.service
    sleep 3
    curl -fsS http://127.0.0.1:8096/health
    echo"

echo
echo "deployed $VERSION -> $RELEASE"
echo "rollback: point $APP_ROOT/current at the previous release and restart the unit"
