BINARY_NAME = src_rust
LOCAL_BINARY = ./src_rust/target/x86_64-unknown-linux-musl/release/$(BINARY_NAME)

.DEFAULT_GOAL := all

all: build check_release_size

build:
	cd src_rust && cargo build --release --target=x86_64-unknown-linux-musl

clean:
	cd src_rust && cargo clean

run:
	$(LOCAL_BINARY)

check_static:
	ldd $(LOCAL_BINARY)

check_release_size:
	du -sh $(LOCAL_BINARY)