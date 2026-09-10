#version 450
layout(binding = 0, std430) readonly buffer Data { uint words[]; } data;
layout(push_constant) uniform Args { uint samples, offset, count, bytes; } args;
layout(location = 0) out uvec2 witness;
void main()
{
    uint value = data.words[uint(gl_FragCoord.x) * args.samples + uint(gl_SampleID)];
#ifdef MIXED
    // Mirrors the production D16/D32 selection; args.bytes is 4 in this probe.
    float depth = args.bytes == 2 ? float(value) / 65535.0 : uintBitsToFloat(value);
#else
    float depth = uintBitsToFloat(value);
#endif
    witness = uvec2(value, floatBitsToUint(depth));
    gl_FragDepth = depth;
}
