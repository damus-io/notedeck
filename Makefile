.DEFAULT_GOAL := check
.PHONY: fake

ANDROID_DIR := crates/notedeck_chrome/android

check:
	cargo check

tags: fake
	rusty-tags vi

jni: fake
	cargo ndk --target arm64-v8a -o $(ANDROID_DIR)/app/src/main/jniLibs/ build --profile release

apk: jni
	cd $(ANDROID_DIR) && ./gradlew build

android: jni
	cd $(ANDROID_DIR) && ./gradlew installDebug
	adb shell am start -n com.damus.notedeck/.MainActivity
	adb logcat -v color | tee logcat.txt
