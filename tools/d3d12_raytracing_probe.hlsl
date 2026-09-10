// Native DXR acceptance library. Compile with dxc -T lib_6_3.
RaytracingAccelerationStructure Scene : register(t0);
RWStructuredBuffer<uint> Results : register(u0);
cbuffer Globals : register(b0) { uint Epoch; }
cbuffer LocalRecord : register(b1) { uint RecordValue; }

struct Payload { uint value; };
struct CallableData { uint value; };

[shader("raygeneration")]
void RayGen()
{
    uint index = DispatchRaysIndex().x;
    RayDesc ray;
    ray.Origin = float3(index == 0 ? 0.0 : index == 2 ? 3.0 : 10.0, 0.0, -1.0);
    ray.Direction = float3(0.0, 0.0, 1.0);
    ray.TMin = 0.0;
    ray.TMax = 10.0;
    Payload payload;
    payload.value = 0xbaad0000;
    TraceRay(Scene, RAY_FLAG_NONE, 1, 0, 1, 0, ray, payload);
    Results[index] = payload.value + Epoch;
}

[shader("miss")]
void Miss(inout Payload payload)
{
    payload.value = 0xdead0000;
}

[shader("closesthit")]
void ClosestHit(inout Payload payload, BuiltInTriangleIntersectionAttributes attributes)
{
    CallableData data;
    data.value = RecordValue;
    CallShader(0, data);
    payload.value = data.value;
}

[shader("intersection")]
void Intersection()
{
    BuiltInTriangleIntersectionAttributes attributes;
    attributes.barycentrics = float2(0.25, 0.5);
    ReportHit(1.25, 0, attributes);
}

[shader("callable")]
void Callable(inout CallableData data)
{
    data.value += RecordValue;
}
