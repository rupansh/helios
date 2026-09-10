/* CPU-only cache rejection tests. Fixtures stay in a private temporary XDG
 * directory; no synthetic PASS result is installed into a real engine cache. */
#define _POSIX_C_SOURCE 200809L
#include <assert.h>
#include <sys/stat.h>
#include <unistd.h>
#include "../vkd3d-proton-helios/include/private/vkd3d_reserved_compat.h"

static void write_record(const char *path, struct vkd3d_sparse_probe_record *record, size_t size)
{
    FILE *file = fopen(path, "wb");
    assert(file);
    record->checksum = vkd3d_sparse_probe_checksum(record);
    assert(fwrite(record, 1, size, file) == size);
    assert(!fclose(file));
}

int main(void)
{
    char root[] = "/tmp/helios-sparse-cache-test-XXXXXX";
    char path[VKD3D_SPARSE_PROBE_PATH_SIZE], parent[VKD3D_SPARSE_PROBE_PATH_SIZE];
    struct vkd3d_sparse_probe_record record = {0}, saved;
    struct vkd3d_sparse_probe_key key = {0};
    FILE *file;
    unsigned i;
    assert(mkdtemp(root));
    assert(!setenv("XDG_CACHE_HOME", root, 1));
    key.format = vkd3d_sparse_probe_formats[4]; key.samples = 4;
    key.device_uuid[0] = 0x12; key.driver_uuid[0] = 0x34;
    assert(vkd3d_sparse_probe_path(path, sizeof(path), &key));
    /* Include the UUID boundary in the filename check (no embedded NUL). */
    assert(strstr(path, "1200000000000000000000000000000034000000000000000000000000000000-37-4.bin"));
    strcpy(parent, path);
    for (i = (unsigned)strlen(root) + 1; parent[i]; i++) if (parent[i] == '/')
    { parent[i] = 0; assert(!mkdir(parent, 0700)); parent[i] = '/'; }
    assert(vkd3d_sparse_probe_read(&key) == VKD3D_SPARSE_PROBE_UNKNOWN);
    record.version = VKD3D_SPARSE_PROBE_VERSION; record.size = sizeof(record); record.key = key;
    record.status = VKD3D_SPARSE_PROBE_PASS; record.completed_phases = 7; record.checked_pixels = 8192;
    saved = record;
    write_record(path, &record, sizeof(record));
    assert(vkd3d_sparse_probe_read(&key) == VKD3D_SPARSE_PROBE_PASS);
    /* Each damaged or stale record must fail closed even with a valid checksum. */
    for (i = 0; i < 12; i++)
    {
        record = saved;
        switch (i)
        {
            case 0: record.version--; break;
            case 1: record.size--; break;
            case 2: record.key.driver_version++; break;
            case 3: record.key.pipeline_uuid[0]++; break;
            case 4: record.key.layer_driver_version++; break;
            case 5: record.key.layer_driver_uuid[0]++; break;
            case 6: record.key.icd_hash_hi++; break;
            case 7: record.key.device_luid[0]++; break;
            case 8: record.completed_phases = 3; break;
            case 9: record.checked_pixels = 0; break;
            case 10: record.bad_pixels = 1; break;
            case 11: record.status = 9; break;
        }
        write_record(path, &record, sizeof(record));
        assert(vkd3d_sparse_probe_read(&key) == VKD3D_SPARSE_PROBE_UNKNOWN);
    }
    record = saved;
    write_record(path, &record, sizeof(record) - 1);
    assert(vkd3d_sparse_probe_read(&key) == VKD3D_SPARSE_PROBE_UNKNOWN);
    write_record(path, &record, sizeof(record));
    file = fopen(path, "ab"); assert(file); assert(fputc(1, file) != EOF); assert(!fclose(file));
    assert(vkd3d_sparse_probe_read(&key) == VKD3D_SPARSE_PROBE_UNKNOWN);
    write_record(path, &record, sizeof(record));
    file = fopen(path, "r+b"); assert(file); assert(!fseek(file, sizeof(record) - 1, SEEK_SET));
    assert(fputc(0, file) != EOF); assert(!fclose(file));
    assert(vkd3d_sparse_probe_read(&key) == VKD3D_SPARSE_PROBE_UNKNOWN);
    record = saved; record.status = VKD3D_SPARSE_PROBE_FAIL;
    write_record(path, &record, sizeof(record));
    assert(vkd3d_sparse_probe_read(&key) == VKD3D_SPARSE_PROBE_FAIL);
    assert(!unlink(path));
    *strrchr(parent, '/') = 0; assert(!rmdir(parent));
    *strrchr(parent, '/') = 0; assert(!rmdir(parent));
    assert(!rmdir(root));
    puts("PASS: 18 cache outcomes, identity/path checks, no live GPU/cache mutations.");
    return 0;
}
