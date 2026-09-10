#if defined(MEASURE)
#ifdef PRODUCER
cbuffer Parameters : register(b0)
{
    uint seed;
    uint count;
    uint2 packet_va;
    uint max_commands;
    uint argument_offset;
    uint cbv_offset;
    uint target_width;
};
RWByteAddressBuffer Packet : register(u0);
[numthreads(64, 1, 1)]
void producer_main(uint3 tid : SV_DispatchThreadID)
{
    uint i = tid.x;
    if (i == 0)
        Packet.Store(0, count);
    // Include a legal suffix CBV for every count, but no extra argument record.
    if (i > max_commands)
        return;
    uint offset = cbv_offset + i * 256;
    Packet.Store3(offset, uint3(seed + 3 * i, 7 * seed + 11 * i, target_width));
    if (i == max_commands)
        return;
    uint lo = packet_va.x + offset;
    uint hi = packet_va.y + uint(lo < packet_va.x);
    uint record = argument_offset + i * 28;
    Packet.Store(record, i);
    Packet.Store2(record + 4, uint2(lo, hi));
    Packet.Store4(record + 12, uint4(3, 1, 0, 0));
}
#else
cbuffer Offset : register(b0) { uint offset; };
cbuffer Values : register(b1)
{
    uint vs_multiplier;
    uint ps_multiplier;
    uint target_width;
};
struct Output
{
    float4 position : SV_Position;
    nointerpolation uint value : TEXCOORD0;
};
Output vertex_main(uint vertex : SV_VertexID)
{
    // The triangle encloses the pixel center and stays inside that pixel's
    // cell. It cannot overlap its neighbor even with viewport roundoff.
    float2 corners[3] = {float2(0.125, 0.125), float2(0.875, 0.125), float2(0.5, 0.875)};
    float2 pixel = float2(float(offset), 0.0) + corners[vertex];
    Output output;
    output.position = float4(pixel.x * (2.0 / float(target_width)) - 1.0,
            1.0 - pixel.y * 2.0, 0.0, 1.0);
    output.value = 2 * vs_multiplier + offset;
    return output;
}
uint pixel_main(Output input) : SV_Target0
{
    return input.value + 3 * ps_multiplier + offset;
}
#endif
#elif defined(PRODUCER)
cbuffer Parameters : register(b0)
{
    uint seed;
    uint count;
    uint2 packet_va;
};
RWByteAddressBuffer Packet : register(u0);

[numthreads(1, 1, 1)]
void producer_main()
{
    Packet.Store2(256, uint2(0, 0));
    Packet.Store2(264, uint2(1, 0));
    Packet.Store(280, count);
    for (uint i = 0; i < 3; i++)
    {
        uint cbv_offset = 512 + i * 256;
        uint lo = packet_va.x + cbv_offset;
        uint hi = packet_va.y + uint(lo < packet_va.x);
        Packet.Store(cbv_offset, seed + i);
#ifdef INPUT_ASSEMBLER
        // Mixed root/IA arguments, tightly packed even at the four-byte base.
        uint record = 284 + i * 64;
        Packet.Store(record, i);
        Packet.Store2(record + 4, uint2(lo, hi));
        uint vb_offset = cbv_offset + 16;
        lo = packet_va.x + vb_offset;
        hi = packet_va.y + uint(lo < packet_va.x);
        Packet.Store4(record + 12, uint4(lo, hi, 12, 4));
        // Distinct fetch values expose a missing BaseVertexLocation adjustment.
        Packet.Store3(vb_offset, uint3(seed + 5 * i, seed + 5 * i + 11, seed + 5 * i + 22));
        uint ib_offset = cbv_offset + 32;
        lo = packet_va.x + ib_offset;
        hi = packet_va.y + uint(lo < packet_va.x);
        bool index16 = ((seed + i) & 1) != 0;
        Packet.Store4(record + 28, uint4(lo, hi, index16 ? 8 : 16, index16 ? 57 : 42));
        if (index16) Packet.Store2(ib_offset, uint2(0x00010000, 0x00030002));
        else Packet.Store4(ib_offset, uint4(0, 1, 2, 3));
        Packet.Store4(record + 44, uint4(3, 1, 1, 0xffffffff));
        Packet.Store(record + 60, 7 + i);
#else
        // Root constant, unaligned 64-bit root CBV, then DRAW; stride 28.
        uint record = 284 + i * 28;
        Packet.Store(record, i);
        Packet.Store2(record + 4, uint2(lo, hi));
        Packet.Store4(record + 12, uint4(3, 1, 0, 0));
#endif
    }
}
#else
cbuffer Multiplier : register(b1) { uint multiplier; };
struct Output
{
    float4 position : SV_Position;
    nointerpolation uint value : TEXCOORD0;
};
Output vertex_main(uint vertex : SV_VertexID
#ifdef INPUT_ASSEMBLER
        , uint vertex_value : COLOR0
#endif
        )
{
    Output output;
#ifdef INPUT_ASSEMBLER
    // SV_VertexID excludes BaseVertexLocation. Indexed IDs 1,2,3 and ordinary
    // IDs 0,1,2 must both cover the pixel, while the VB fetch still uses -1.
    vertex %= 3;
#endif
    output.position = float4(float(vertex & 1) * 4.0 - 1.0,
            float(vertex & 2) * 2.0 - 1.0, 0.0, 1.0);
    output.value = multiplier;
#ifdef INPUT_ASSEMBLER
    output.value += vertex_value;
#endif
    return output;
}
cbuffer Offset : register(b0) { uint offset; };
RWStructuredBuffer<uint> Accumulator : register(u0);
struct Input
{
    float4 position : SV_Position;
    nointerpolation uint value : TEXCOORD0;
};
void pixel_main(Input input)
{
    InterlockedAdd(Accumulator[offset], input.value + multiplier);
}
#endif
