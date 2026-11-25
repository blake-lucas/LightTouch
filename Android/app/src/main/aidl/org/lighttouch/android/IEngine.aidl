package org.lighttouch.android;
import android.os.Bundle;
import android.os.ParcelFileDescriptor;
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
    /** Write a stopped device to [destination] as a device file (a zip another LightTouch imports): the error or "".
     *  Progress through importProgress(); cancelTransfer() stops it. */
    String exportDevice(String recordId, in ParcelFileDescriptor destination, String app);
    /** Add the device in the device file [input] as a new device: error ("" when it worked), id and name. */
    Bundle importDevice(in ParcelFileDescriptor input);
    void cancelTransfer();
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
    /** Settings > Microphone, already checked against the RECORD_AUDIO permission. */
    void setMicrophone(boolean enabled);
    /** Settings > Vibration: the phone vibrates while the guest runs its vibration motor. */
    void setVibration(boolean enabled);
    /** End the worker now, without waiting for iOS to shut down (the Stop deadline's disposal, early). */
    void forceStop();
    Bundle hitchSnapshot();
    /** The iOS app in front: bundleId and appName, both "" when the guest tools cannot say (a paused guest). */
    Bundle frontmostApp();
    /** Reload the atomic saved network document and apply to this session. */
    void applyNetworkSettings();
    /** A device's carrier settings (devices with a modem): carrier, mccMnc, registered, simPresent, bars, latitude,
     *  longitude, lastCaller, lastSender, and error ("" when read). */
    Bundle carrier(String recordId);
    /** Check and save them; this worker's running session takes what changed. Returns the error or "". */
    String setCarrier(String recordId, in Bundle settings);
    /** A network-side event for the running modem: incoming-call, remote-answer, remote-hangup, incoming-sms. */
    String modem(String action, String value);
    /** The running modem's state as JSON, "" when there is none. */
    String modemStatus();
    /** Hold the engine's periodic storage sync while an app installs (true), or release it: lighttouch.h. */
    oneway void holdStorageSync(boolean hold);
}
