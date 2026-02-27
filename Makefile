.DEFAULT_GOAL := check
.PHONY: fake

ANDROID_DIR := crates/notedeck_chrome/android

check:
	cargo check

tags: fake
	rusty-tags vi

jni: fake
	cargo ndk --target arm64-v8a -o $(ANDROID_DIR)/app/src/main/jniLibs/ build --features messages --profile release

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
