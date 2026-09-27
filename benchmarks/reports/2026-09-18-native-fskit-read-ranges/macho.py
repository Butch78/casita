"""Retain file ranges from the measured thin, little-endian Mach-O binaries."""
import hashlib
import json
from pathlib import Path
import struct
import sys


def layout(path):
    data = Path(path).read_bytes()
    magic, _, _, _, ncmds, sizeofcmds, _, _ = struct.unpack_from('<8I', data)
    assert magic == 0xfeedfacf, 'expected thin little-endian Mach-O 64'
    ranges = [{'kind':'header-and-commands', 'offset':0, 'size':32+sizeofcmds}]
    offset = 32
    for _ in range(ncmds):
        cmd, size = struct.unpack_from('<2I', data, offset)
        assert size >= 8 and offset+size <= 32+sizeofcmds
        if cmd == 0x19:  # LC_SEGMENT_64
            name, _, _, fileoff, filesize, _, _, nsects, _ = struct.unpack_from('<16s4Q4I', data, offset+8)
            name = name.rstrip(b'\0').decode()
            ranges.append({'kind':'segment', 'name':name, 'offset':fileoff, 'size':filesize})
            for i in range(nsects):
                section, segment, _, length, fileoffset = struct.unpack_from('<16s16sQQI', data, offset+72+i*80)
                flags, = struct.unpack_from('<I', data, offset+72+i*80+64)
                ranges.append({'kind':'section', 'name':segment.rstrip(b'\0').decode()+','+section.rstrip(b'\0').decode(),
                               'offset':fileoffset, 'size':length,
                               'file_backed':flags & 0xff not in (1, 0xc, 0x12)})
        elif cmd == 0x1d:  # LC_CODE_SIGNATURE
            fileoff, filesize = struct.unpack_from('<2I', data, offset+8)
            ranges.append({'kind':'code-signature', 'offset':fileoff, 'size':filesize})
        offset += size
    assert offset == 32+sizeofcmds
    return {'source':str(path), 'sha256':hashlib.sha256(data).hexdigest(), 'size':len(data), 'ranges':ranges}


if __name__ == '__main__':
    print(json.dumps([layout(path) for path in sys.argv[1:]], indent=2))
