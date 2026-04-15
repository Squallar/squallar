# Dark Theme Detection for Android

This is a dumb module I didn't want to write. It detects whether the Android system theme is dark or light. It uses unsafe code and JNI to call into Android APIs.

The reason this has to be separate from `rustdar-android` is to avoid a cyclic dependency, because `rustdar-platform` would need to call into this to detect the theme, and `rustdar-android` depends on `rustdar-platform`, as it's the Android entry point.
