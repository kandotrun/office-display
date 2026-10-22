# Tasks for local development. The display itself is deployed with deploy/*.sh.

scratch := env_var_or_default("OFFICE_DISPLAY_DEV", "/tmp/office-display-dev")
py := "uv run --no-project --with numpy --with pillow"
# The office position comes from the environment, never from the repository.
lat := env_var_or_default("ATLAS_LAT", "35.68")
lon := env_var_or_default("ATLAS_LON", "139.77")

default: test

# Run every test suite
test: test-rust test-atlas

test-rust:
    cd photo-frame-rs && cargo test --locked

test-atlas:
    {{py}} --with pytest python -m pytest atlas/tests -q

# Generate sample photos (some with GPS) and serve them like photo-manager on :8081
dev-photos count="12":
    {{py}} python dev/make_sample_photos.py {{scratch}}/photos {{count}}
    {{py}} python dev/fake_photo_manager.py {{scratch}}/photos

# Run atlas on :8099 against the real data sources with a local cache
dev-atlas:
    cd atlas && ATLAS_LAT={{lat}} ATLAS_LON={{lon}} ATLAS_CACHE_DIR={{scratch}}/atlas-cache {{py}} python atlas_server.py

# Run the photo frame in a window
dev-frame *args:
    cd photo-frame-rs && cargo run --release --locked -- --lat {{lat}} --lon {{lon}} {{args}}
