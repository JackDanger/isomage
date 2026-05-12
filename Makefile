# isomage is a library crate; cargo handles building it. The Makefile
# only owns the synthetic ISO images that drive the integration tests.
# Generate them once with `make test-data`; rebuild from scratch with
# `make clean-test-data && make test-data`.

.PHONY: build test test-data clean clean-test-data

# Convenience wrappers — `cargo` is the canonical interface.
build:
	cargo build

test: test-data
	cargo test

clean:
	cargo clean

# Test data generation
test-data: test_data/test_linux.iso test_data/test_macos.iso

# Create test_linux.iso
test_data/test_linux.iso:
	@echo "Creating test_linux.iso..."
	@mkdir -p test_data/linux_source/boot
	@mkdir -p test_data/linux_source/etc
	@mkdir -p test_data/linux_source/home/user
	@mkdir -p test_data/linux_source/usr/bin
	@mkdir -p test_data/linux_source/var/log
	@echo "GRUB boot loader" > test_data/linux_source/boot/grub.cfg
	@echo "test-linux-system" > test_data/linux_source/etc/hostname
	@echo "127.0.0.1 localhost" > test_data/linux_source/etc/hosts
	@echo "# Bash configuration" > test_data/linux_source/home/user/.bashrc
	@echo -e "#!/bin/bash\necho \"Hello World\"" > test_data/linux_source/usr/bin/hello
	@echo "System started" > test_data/linux_source/var/log/system.log
	@chmod +x test_data/linux_source/usr/bin/hello
	mkisofs -r -J -o test_data/test_linux.iso test_data/linux_source 2>/dev/null || \
		(hdiutil makehybrid -iso -joliet -o test_data/test_linux test_data/linux_source && mv test_data/test_linux.iso test_data/test_linux.iso) 2>/dev/null || \
		genisoimage -r -J -o test_data/test_linux.iso test_data/linux_source
	@echo "test_linux.iso created successfully"

# Create test_macos.iso
test_data/test_macos.iso:
	@echo "Creating test_macos.iso..."
	@mkdir -p test_data/macos_source/Applications
	@mkdir -p test_data/macos_source/System/Library
	@mkdir -p test_data/macos_source/Users/user
	@mkdir -p test_data/macos_source/private/var/log
	@echo "Welcome to macOS" > test_data/macos_source/Users/user/welcome.txt
	@echo "System Library Files" > test_data/macos_source/System/Library/info.txt
	@echo "Application Data" > test_data/macos_source/Applications/readme.txt
	@echo "macOS system log" > test_data/macos_source/private/var/log/system.log
	mkisofs -r -J -o test_data/test_macos.iso test_data/macos_source 2>/dev/null || \
		(hdiutil makehybrid -iso -joliet -o test_data/test_macos test_data/macos_source && mv test_data/test_macos.iso test_data/test_macos.iso) 2>/dev/null || \
		genisoimage -r -J -o test_data/test_macos.iso test_data/macos_source
	@echo "test_macos.iso created successfully"

# Clean test data
clean-test-data:
	rm -rf test_data/linux_source test_data/macos_source
	rm -f test_data/test_linux.iso test_data/test_macos.iso
