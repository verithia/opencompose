/**
 * Sentences the audio-IO section prints for a negotiated format, a failed
 * action or a pending control. One module so the Preferences page and any
 * other surface word the same state identically.
 */
import { i18n } from "./i18n";
import type { AudioFailure, AudioNotice, AudioPending } from "./audioIo";

/** One negotiated-format notice, in the current locale. */
export function audioNoticeText(notice: AudioNotice): string {
  switch (notice.kind) {
    case "missing_device":
      return i18n.t("toolbar.audioMissingDevice", { name: notice.name, fallback: "—" });
    case "rate_substituted":
      return i18n.t("toolbar.audioRateSubstituted", {
        requested: notice.requested,
        chosen: notice.chosen,
      });
    case "buffer_unsupported":
      return i18n.t("toolbar.audioBufferUnsupported", { requested: notice.requested });
    case "buffer_clamped":
      return i18n.t("toolbar.audioBufferClamped", {
        requested: notice.requested,
        chosen: notice.chosen,
        min: notice.min,
        max: notice.max,
      });
    case "buffer_rejected":
      return i18n.t("toolbar.audioBufferRejected", { requested: notice.requested });
    case "settings_unreadable":
      return i18n.t("toolbar.audioSettingsUnreadable");
    case "settings_unwritable":
      return i18n.t("toolbar.audioSettingsUnwritable", { error: notice.error });
    case "stream_failed":
      return i18n.t("toolbar.audioStreamFailed", { role: notice.role, error: notice.error });
    case "missing_input_device":
      return i18n.t("toolbar.audioInputMissingDevice", { name: notice.name, fallback: "—" });
    case "input_rate_substituted":
      return i18n.t("toolbar.audioInputRateSubstituted", {
        requested: notice.requested,
        chosen: notice.chosen,
      });
    case "input_channels_reduced":
      return i18n.t("toolbar.audioInputChannelsReduced", {
        requested: notice.requested,
        chosen: notice.chosen,
      });
    case "input_failed":
      return i18n.t("toolbar.audioInputFailed", { error: notice.error });
  }
}

/** One failed action, in the current locale. */
export function audioFailureText(failure: AudioFailure): string {
  switch (failure.kind) {
    case "recording_active":
      return i18n.t("toolbar.audioRecordingActive");
    case "no_devices":
      return i18n.t("toolbar.audioMissingNoFallback");
    case "list_failed":
      return i18n.t("toolbar.audioListFailed", { error: failure.error });
    case "open_failed":
      return failure.fallback
        ? i18n.t("toolbar.audioOpenFailed", {
            device: failure.device,
            error: failure.error,
            fallback: failure.fallback,
          })
        : i18n.t("toolbar.audioOpenFailedNoFallback", {
            device: failure.device,
            error: failure.error,
          });
    case "stream_lost":
      return i18n.t("toolbar.audioStreamLost", {
        device: failure.device,
        error: failure.error,
      });
    case "no_input_devices":
      return i18n.t("toolbar.audioInputMissingNoFallback");
    case "input_open_failed":
      return failure.fallback
        ? i18n.t("toolbar.audioOpenFailed", {
            device: failure.device,
            error: failure.error,
            fallback: failure.fallback,
          })
        : i18n.t("toolbar.audioInputOpenFailed", {
            device: failure.device,
            error: failure.error,
          });
  }
}

/** One capture action's failure, in the current locale. */
export function audioInputFailureText(failure: AudioFailure): string {
  switch (failure.kind) {
    case "no_input_devices":
      return i18n.t("toolbar.audioInputMissingNoFallback");
    case "input_open_failed":
      return failure.fallback
        ? i18n.t("toolbar.audioOpenFailed", {
            device: failure.device,
            error: failure.error,
            fallback: failure.fallback,
          })
        : i18n.t("toolbar.audioInputOpenFailed", {
            device: failure.device,
            error: failure.error,
          });
    default:
      return audioFailureText(failure);
  }
}

/** The pending label of the control that is waiting. */
export function audioPendingText(pending: NonNullable<AudioPending>): string {
  switch (pending.kind) {
    case "output":
      return i18n.t("toolbar.audioSwitching", { name: pending.detail });
    case "rate":
      return i18n.t("toolbar.audioReopeningRate", { rate: pending.detail });
    case "buffer":
      return i18n.t("toolbar.audioReopeningBuffer", { frames: pending.detail });
    case "refresh":
      return i18n.t("toolbar.audioRefreshing");
    case "input":
      return i18n.t("toolbar.audioInputSwitching", { name: pending.detail });
    case "channels":
      return i18n.t("toolbar.audioInputChannelsPending", {
        channels: i18n.t(pending.detail === "stereo" ? "toolbar.audioInputStereo" : "toolbar.audioInputMono"),
      });
    case "monitor":
      return i18n.t("toolbar.audioMonitorPending");
  }
}

/** Latency readout: milliseconds and the frames they come from. */
export function audioLatencyValue(seconds: number, frames: number): string {
  return i18n.t("toolbar.audioLatencyValue", {
    ms: (seconds * 1000).toFixed(1),
    frames,
  });
}
