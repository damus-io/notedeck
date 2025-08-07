.DEFAULT_GOAL := check
.PHONY: fake

ANDROID_DIR := crates/notedeck_chrome/android

check:
	cargo check

tags: fake
	rusty-tags vi

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

android: jni
	cd $(ANDROID_DIR) && ./gradlew installDebug
	adb shell am start -n com.damus.notedeck/.MainActivity
	adb logcat -v color -s GameActivity -s RustStdoutStderr -s threaded_app | tee logcat.txt
