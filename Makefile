.DEFAULT_GOAL := check
.PHONY: fake

ANDROID_DIR := crates/notedeck_chrome/android
MACOS_APP := target/release/bundle/osx/Notedeck.app
MACOS_FEATURES ?= dave,messages

check:
	cargo check

tags: fake
	rusty-tags vi

jni: fake
	cargo ndk --target arm64-v8a -o $(ANDROID_DIR)/app/src/main/jniLibs/ build --features dave,messages,headway,notebook,auto-update --profile release --workspace --exclude notedeck_release

jni-check: fake
	cargo ndk --target arm64-v8a check --workspace --exclude notedeck_release

apk: jni
	cd $(ANDROID_DIR) && ./gradlew build

gradle:
	cd $(ANDROID_DIR) && ./gradlew build

push-android-config:
	adb push android-config.json /sdcard/Android/data/com.damus.notedeck/files/android-config.json

android: jni
	cd $(ANDROID_DIR) && ./gradlew installDebug
	adb shell am start -n com.damus.notedeck/.MainActivity
	adb logcat -v color -s GameActivity -s RustStdoutStderr -s threaded_app | tee logcat.txt

android-release: jni
	cd $(ANDROID_DIR) && ./gradlew installRelease
	adb shell am start -n com.damus.notedeck/.MainActivity
	adb logcat -v color -s GameActivity -s RustStdoutStderr -s threaded_app | tee logcat.txt

release-apk: jni
	cd $(ANDROID_DIR) && ./gradlew assembleRelease
	@echo "Signed APK: $(ANDROID_DIR)/app/build/outputs/apk/release/app-release.apk"

release-aab: jni
	cd $(ANDROID_DIR) && ./gradlew bundleRelease
	@echo "Signed AAB: $(ANDROID_DIR)/app/build/outputs/bundle/release/app-release.aab"

android-tracy: fake
	cargo ndk --target arm64-v8a -o $(ANDROID_DIR)/app/src/main/jniLibs/ build --profile release --features tracy
	cd $(ANDROID_DIR) && ./gradlew installDebug
	adb shell am start -n com.damus.notedeck/.MainActivity
	adb forward tcp:8086 tcp:8086
	adb logcat -v color -s GameActivity -s RustStdoutStderr -s threaded_app | tee logcat.txt

# Build an unsigned .app for local development. macOS reads a process's icon
# and name from the bundle's Info.plist, so a bare target/release binary shows
# up as a generic executable in Activity Monitor and cmd-tab. For the signed,
# notarized release build see scripts/macos_build.sh.
macos-app: fake
	@command -v cargo-bundle >/dev/null || \
		{ echo "cargo-bundle not found; install it with: cargo install cargo-bundle"; exit 1; }
	cargo bundle -p notedeck_chrome --release --format osx --features "$(MACOS_FEATURES)"
	@echo "Built $(MACOS_APP)"

# Run the bundled build with logs still going to the terminal. Launching the
# inner executable rather than `open`ing the .app keeps stdout attached while
# still giving the process its bundle identity.
macos-run: macos-app
	$(MACOS_APP)/Contents/MacOS/notedeck

test-messages-docker:
	docker build -f crates/notedeck_testing/Dockerfile -t notedeck-test-base .
	docker run --rm \
	  --cpus=2 --memory=7g \
	  -v "$$PWD":/work -w /work \
	  -v cargo-registry:/root/.cargo/registry \
	  -v cargo-git:/root/.cargo/git \
	  -v cargo-target:/target \
	  -e CARGO_TARGET_DIR=/target \
	  notedeck-test-base bash -lc '$${STRESS_CMD:+stress-ng --cpu 2 --cpu-load $${STRESS_CPU_LOAD:-70} &} cargo test -p notedeck_messages --test messages_e2e -- --test-threads=1'
