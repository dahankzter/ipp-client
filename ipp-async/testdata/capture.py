#!/usr/bin/env python3
"""Captures a raw CUPS-Get-Printers response from the local cupsd."""
import struct, sys, urllib.request

def attr(tag, name, val):
    return struct.pack('>BH', tag, len(name)) + name + struct.pack('>H', len(val)) + val

body = struct.pack('>BBHI', 2, 0, 0x4002, 1) + b'\x01'
body += attr(0x47, b'attributes-charset', b'utf-8')
body += attr(0x48, b'attributes-natural-language', b'en')
body += b'\x03'

req = urllib.request.Request(
    'http://localhost:631/', data=body, headers={'Content-Type': 'application/ipp'})
sys.stdout.buffer.write(urllib.request.urlopen(req, timeout=10).read())
