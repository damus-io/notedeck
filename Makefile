.DEFAULT_GOAL := check
.PHONY: fake

ANDROID_DIR := crates/notedeck_chrome/android
ARTI_BUILD_DIR := tools/arti-build

check:
	cargo check

tags: fake
	rusty-tags vi

# Build Arti Tor native library for Android (all architectures)
arti: fake
	cd $(ARTI_BUILD_DIR) && ./build-arti.sh

# Build Arti for ARM64 only (faster for development)
arti-arm64: fake
	cd $(ARTI_BUILD_DIR) && ./build-arti.sh --release

# Clean Arti build artifacts
arti-clean: fake
	cd $(ARTI_BUILD_DIR) && ./build-arti.sh --clean

jni: fake
	cargo ndk --target arm64-v8a -o $(ANDROID_DIR)/app/src/main/jniLibs/ build --profile release

jni-check: fake
	cargo ndk --target arm64-v8a check

apk: jni
	cd $(ANDROID_DIR) && ./gradlew build

gradle:
	cd $(ANDROID_DIR) && ./gradlew build

push-android-config:
	adb push android-config.json /sdcard/Android/data/com.damus.notedeck/files/android-config.json

# Build and install Android app (without Tor - use android-tor for Tor support)
android: jni
	cd $(ANDROID_DIR) && ./gradlew installDebug
	adb shell am start -n com.damus.notedeck/.MainActivity
	adb logcat -v color -s GameActivity -s RustStdoutStderr -s threaded_app | tee logcat.txt

# Build and install Android app with Tor support
android-tor: arti-arm64 jni
	cd $(ANDROID_DIR) && ./gradlew installDebug
	adb shell am start -n com.damus.notedeck/.MainActivity
	adb logcat -v color -s GameActivity -s RustStdoutStderr -s threaded_app -s NativeTorProvider -s TorManager | tee logcat.txt

android-tracy: fake
	cargo ndk --target arm64-v8a -o $(ANDROID_DIR)/app/src/main/jniLibs/ build --profile release --features tracy
	cd $(ANDROID_DIR) && ./gradlew installDebug
	adb shell am start -n com.damus.notedeck/.MainActivity
	adb forward tcp:8086 tcp:8086
	adb logcat -v color -s GameActivity -s RustStdoutStderr -s threaded_app | tee logcat.txt
