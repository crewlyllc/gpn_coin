import struct, json, urllib.request, math, sys

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

def q(method,params):
    req=urllib.request.Request("http://127.0.0.1:9944",data=json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode(),headers={"Content-Type":"application/json"})
    return json.load(urllib.request.urlopen(req,timeout=8)).get("result")

def difficulty_key():
    return "0x"+(twox128(b"GPUPoW")+twox128(b"Difficulty")).hex()

if __name__ == "__main__":
    if len(sys.argv)>1 and sys.argv[1]=="key":
        print(difficulty_key()); sys.exit()
    if len(sys.argv)>1 and sys.argv[1]=="set":
        # set: arg = decimal or 0x value (256-bit target)
        v=int(sys.argv[2],0)
        b=v.to_bytes(32,'little')
        print("SETKEY", difficulty_key())
        print("SETVAL", "0x"+b.hex())
        sys.exit()
    key=difficulty_key()
    val=q("state_getStorage",[key])
    print("Difficulty:",val)
    if val and val!="0x":
        b=bytes.fromhex(val[2:]); limbs=[int.from_bytes(b[i*8:i*8+8],'little') for i in range(4)]
        print("limbs",[hex(x) for x in limbs]); v=int.from_bytes(b,'little'); print("~2^%.2f"%math.log2(v))
