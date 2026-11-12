/**
 * Which beat position the playhead paints.
 *
 * The transport poll answers twenty times a second while the store also
 * interpolates between polls. A playing transport paints the interpolated
 * value, so the head moves with the audio; a parked one paints the reported
 * position, which is the only exact answer when nothing is advancing.
 */
export function cursorBeat(playing: boolean, smooth: number, reported: number): number {
  return playing ? smooth : reported;
}
