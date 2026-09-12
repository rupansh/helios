// Native SO acceptance shaders. Both PACKED float2 values are eligible for
// register packing; the declaration captures only part of the first value.
struct Vertex {
    float4 position : SV_Position;
    float2 first : PACKED0;
    float2 second : PACKED1;
};
// SV_VertexID excludes StartVertexLocation. Explicit tags keep distinct draws
// distinguishable for the ordering/reuse checks without changing that rule.
cbuffer DrawData : register(b0) { uint draw_tag; };
Vertex vs_main(uint id : SV_VertexID) {
    id += draw_tag;
    Vertex v;
    v.position = float4(0.0, 0.0, 0.5, 1.0);
    v.first = float2(10 + id, 20 + id);
    v.second = float2(30 + id, 40 + id);
    return v;
}
[maxvertexcount(2)]
void gs_main(point Vertex input[1], inout PointStream<Vertex> first,
             inout PointStream<Vertex> second) {
    Vertex v = input[0];
    first.Append(v);
    v.first += 100;
    v.second += 100;
    second.Append(v);
}
float4 ps_main(Vertex v) : SV_Target { return float4(v.first.x / 255.0, v.second.x / 255.0, v.second.y / 255.0, 1); }
