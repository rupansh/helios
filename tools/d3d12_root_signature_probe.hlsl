cbuffer Values : register(b0) { uint4 words[16]; };
RWByteAddressBuffer Output : register(u0);
[numthreads(1, 1, 1)]
void main() {
    Output.Store4(0, uint4(words[0].x, words[7].w, words[15].y, 0x37e25a91));
}
