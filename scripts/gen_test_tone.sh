#!/usr/bin/env bash
# Generates the 3s stereo test tone used by musico_playback/tests/engine_e2e.rs
python3 - <<'PY'
import wave, struct, math
sr = 44100
with wave.open('/tmp/musico-test-tone.wav', 'w') as w:
    w.setnchannels(2); w.setsampwidth(2); w.setframerate(sr)
    frames = bytearray()
    for i in range(sr * 3):
        l = int(32767 * 0.8 * math.sin(2 * math.pi * 440 * i / sr))
        r = int(32767 * 0.8 * math.sin(2 * math.pi * 660 * i / sr))
        frames += struct.pack('<hh', l, r)
    w.writeframes(bytes(frames))
print("wrote /tmp/musico-test-tone.wav")
PY
