#!/usr/bin/env python3
"""Package the pinned OpenSSL notice used by the shared network backend."""
import argparse,hashlib,json,pathlib,tarfile
p=argparse.ArgumentParser();p.add_argument('destination',type=pathlib.Path);p.add_argument('--prefix',type=pathlib.Path,required=True);p.add_argument('--pins',type=pathlib.Path,required=True);a=p.parse_args()
pin=next(x for x in json.loads(a.pins.read_text())['packages'] if x['name']=='openssl')
archive=a.prefix.parent/pin['archive']
assert hashlib.sha256(archive.read_bytes()).hexdigest()==pin['sha256'],'OpenSSL source archive differs from dependency pin'
a.destination.mkdir(parents=True,exist_ok=True)
with tarfile.open(archive) as tar:
 member=next(x for x in tar.getmembers() if x.name.endswith('/LICENSE.txt') and len(x.name.split('/'))==2)
 data=tar.extractfile(member).read()
(a.destination/'OpenSSL-LICENSE.txt').write_bytes(data)
manifest={'schema':1,'source_reference':pin['url'],'files':[{'path':'OpenSSL-LICENSE.txt','sha256':hashlib.sha256(data).hexdigest()}],
 'components':[{'name':'OpenSSL (shared network backend)','archive_sha256':pin['sha256'],'static_libraries':{name:hashlib.sha256((a.prefix/'lib'/name).read_bytes()).hexdigest() for name in ('libssl.a','libcrypto.a')}}]}
(a.destination/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
