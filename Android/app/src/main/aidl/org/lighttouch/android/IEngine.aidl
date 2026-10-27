package org.lighttouch.android;
import android.os.Bundle;
import android.view.Surface;
interface IEngine {
    Bundle start(String recordId);
    String debugArmQmp();
    String debugSelectGraphicsProvider(int choice);
    Bundle debugQmpSnapshot();
    Bundle[] records();
    String select(String recordId);
    String importPrepared(String source, String displayName, String rom);
    /** importPrepared for this app's own firmwarekit output: moved in as the base, not copied or hashed again. */
    String adoptPrepared(String source, String displayName, String rom);
    Bundle importProgress();
    String setGuestPackage(String recordId, String path);
    Bundle status();
    Bundle profile(boolean recording);
    long claimDisplay();
    String attachDisplay(long lease, in Surface surface);
    void detachDisplay(long lease);
    void releaseDisplay(long lease);
    void cancelDisplayInput(long lease);
    void backgroundDisplay(long lease);
    String touchDisplay(long lease, long inputGeneration, long timestampNanos, in int[] ids, in int[] phases, in float[] coordinates, int width, int height, int rotation);
    void pause(boolean paused);
    void stop();
    void button(int button, boolean down);
    String touch(long timestampNanos, in int[] ids, in int[] phases, in float[] coordinates, int width, int height, int rotation);
    void cancelInput();
    oneway void motion(int kind, long timestampNanos, in float[] xyz, int rotation);
    String screenshot(String path);
    Bundle guestRequest(int operation, String argument, int timeoutMs);
    Bundle guestPoll(long requestId);
    void guestCancel(long requestId);
    String debugEnableTouchTrace();
    Bundle recoverStart(String recordId);
    long displayInputGeneration(long lease);
    Bundle performance(long observationEpoch);
    String debugArmVcpuAccounting();
    Bundle debugShutdownProgress();
    String debugSelectTcgIrCache(boolean enabled);
    String rename(String recordId, String name);
    String remove(String recordId);
    Bundle frameStats(long sinceSequence);
    void setPresentScale(int mode);
    void setPresentVisible(boolean visible);
    void setDetailedTiming(boolean enabled);
    Bundle hitchSnapshot();
    /** The iOS app in front: bundleId and appName, both "" when the guest tools cannot say (a paused guest). */
    Bundle frontmostApp();
    /** Reload the atomic saved network document and apply to this session. */
    void applyNetworkSettings();
}
