// Minimal Android 15 (API 35) IServiceManager AIDL.
// Android 15 added getService2() between getService and checkService compared
// to Android 14, which shifts transaction codes. rsbinder 0.6 doesn't have an
// Android 15 variant, so we generate our own bindings with the correct ordering.
// Only addService is used; the other methods are stubs for correct transaction
// code alignment.

package android.os;

interface IServiceManager {
    @nullable IBinder getService(@utf8InCpp String name);

    // Added in Android 15 — shifts all subsequent transaction codes by 1.
    @nullable IBinder getService2(@utf8InCpp String name);

    @nullable IBinder checkService(@utf8InCpp String name);

    void addService(@utf8InCpp String name, IBinder service,
        boolean allowIsolated, int dumpPriority);
}
