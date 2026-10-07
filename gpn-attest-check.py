import hashlib, struct, json, urllib.request

ALPHA="123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
def b58decode(s):
    n=0
    for c in s: n=n*58+ALPHA.index(c)
    b=n.to_bytes((n.bit_length()+7)//8,'big')
    pad=len(s)-len(s.lstrip('1'))
    return b'\x00'*pad+b

def xxh64(data, seed=0):
    P1=0x9E3779B185EBCA87;P2=0xC2B2AE3D27D4EB4F;P3=0x165667B19E3779F9;P4=0x85EBCA77C2B2AE63;P5=0x27D4EB2F165667C5
    M=0xFFFFFFFFFFFFFFFF
    def rotl(x,r): return ((x<<r)|(x>>(64-r)))&M
    n=len(data); i=0
    if n>=32:
        v1=(seed+P1+P2)&M;v2=(seed+P2)&M;v3=seed&M;v4=(seed-P1)&M
        while i+32<=n:
            v1=(rotl((v1+int.from_bytes(data[i:i+8],'little')*P2)&M,31)*P1)&M
            v2=(rotl((v2+int.from_bytes(data[i+8:i+16],'little')*P2)&M,31)*P1)&M
            v3=(rotl((v3+int.from_bytes(data[i+16:i+24],'little')*P2)&M,31)*P1)&M
            v4=(rotl((v4+int.from_bytes(data[i+24:i+32],'little')*P2)&M,31)*P1)&M
            i+=32
        h=(rotl(v1,1)+rotl(v2,7)+rotl(v3,12)+rotl(v4,18))&M
        for v in (v1,v2,v3,v4):
            v=(rotl((v*P2)&M,31)*P1)&M
            h=((h^v)*P1+P4)&M
    else:
        h=(seed+P5)&M
    h=(h+n)&M
    while i+8<=n:
        h=(rotl(h^((rotl((int.from_bytes(data[i:i+8],'little')*P2)&M,31)*P1)&M),27)*P1+P4)&M;i+=8
    if i+4<=n:
        h=(rotl(h^((int.from_bytes(data[i:i+4],'little')*P1)&M),23)*P2+P3)&M;i+=4
    while i<n:
        h=(rotl(h^(data[i]*P5&M),11)*P1)&M;i+=1
    h^=h>>33;h=(h*P2)&M;h^=h>>29;h=(h*P3)&M;h^=h>>32
    return h
def twox128(d): return struct.pack('<Q',xxh64(d,0))+struct.pack('<Q',xxh64(d,1))
def b2_128(d): return hashlib.blake2b(d, digest_size=16).digest()

def q(method,params,url="http://127.0.0.1:9944"):
    req=urllib.request.Request(url,data=json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode(),headers={"Content-Type":"application/json"})
    return json.load(urllib.request.urlopen(req,timeout=10)).get("result")

def ss58_pubkey(s):
    b=b58decode(s)
    # 1-byte prefix -> 35 bytes (prefix+32+2); 2-byte -> 36
    if len(b)==35: return b[1:33]
    if len(b)==36: return b[2:34]
    return b[1:-2]

def att_record(pub):
    key="0x"+(twox128(b"GPUAttestation")+twox128(b"Attestations")+b2_128(pub)+pub).hex()
    return q("state_getStorage",[key])

for label,ss in [("provider-5ENqSp5Q","5ENqSp5Q65bxfgryZExQDRnV1rEW1qb2UBztvswLamkoUrqj")]:
    pub=ss58_pubkey(ss)
    print(label, "pub=0x"+pub.hex())
    v=att_record(pub)
    print("  att storage:", v)
    if v and v!="0x":
        b=bytes.fromhex(v[2:])
        # submitted_at u32(0..4), gpu_info_hash[32](4..36), challenge[32](36..68),
        # gpu_count u8(68), compute_gflops u64(69..77), tier enum(77), missed u32
        gpu_count=b[68]
        gflops=int.from_bytes(b[69:77],'little')
        tier=b[77]
        print(f"  gpu_count={gpu_count} compute_gflops={gflops} tier={tier}")
