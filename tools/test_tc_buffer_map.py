#!/usr/bin/env python3
"""Exercise Mesa's actual tc_buffer_map body with a failing fake pipe driver.

This narrow contract test needs a C compiler, not a complete Mesa build. It
does not validate ABI layout, the worker queue, or the Windows ICD. Use
--revision a04516a702dff81d3a2e44019cdd79abf3fb7423 to demonstrate the failure
against the original Mesa revision.
"""
import argparse
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
MESA = ROOT / "icd/mesa"
SOURCE = "src/gallium/auxiliary/util/u_threaded_context.c"
PRELUDE = r"""
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
enum { PIPE_MAP_THREAD_SAFE=1, TC_TRANSFER_MAP_UPLOAD_CPU_STORAGE=2,
       PIPE_RESOURCE_FLAG_DONT_MAP_DIRECTLY=4, PIPE_MAP_READ=8,
       PIPE_MAP_DISCARD_RANGE=16, PIPE_MAP_UNSYNCHRONIZED=32,
       TC_TRANSFER_MAP_THREADED_UNSYNC=64 };
struct pipe_resource { unsigned width0, flags; };
struct pipe_box { int x, width; };
struct pipe_transfer {
   struct pipe_resource *resource;
   unsigned usage, level, offset, stride, layer_stride;
   struct pipe_box box;
};
struct range { unsigned start, end; };
struct threaded_transfer {
   struct pipe_transfer b;
   struct range *valid_buffer_range;
   bool cpu_storage_mapped;
   void *staging;
};
struct pipe_context {
   void *(*buffer_map)(struct pipe_context *, struct pipe_resource *, unsigned,
                      unsigned, const struct pipe_box *, struct pipe_transfer **);
   void (*buffer_unmap)(struct pipe_context *, struct pipe_transfer *);
   void *stream_uploader;
};
struct threaded_resource {
   struct pipe_resource b;
   bool allow_cpu_storage;
   void *cpu_storage;
   struct pipe_resource *latest;
   struct range valid_buffer_range, pending_staging_uploads_range;
   unsigned pending_staging_uploads;
};
struct threaded_context {
   struct pipe_context base, *pipe;
   unsigned map_buffer_alignment, pool_transfers, bytes_mapped_estimate;
   bool use_forced_staging_uploads;
};
#define threaded_context(p) ((struct threaded_context *)(p))
#define threaded_resource(p) ((struct threaded_resource *)(p))
#define threaded_transfer(p) ((struct threaded_transfer *)(p))
#define p_atomic_read(p) (*(p))
#define p_atomic_inc(p) (++*(p))
static int depth, entered, cleared, maps, unmaps, allocations, fail_map;
static unsigned char gpu_data[128];
static struct threaded_transfer driver_transfer;
static struct pipe_resource *expected_resource;
static void tc_set_driver_thread(struct threaded_context *tc)
{ (void)tc; assert(depth++ == 0); ++entered; }
static void tc_clear_driver_thread(struct threaded_context *tc)
{ (void)tc; assert(--depth == 0); ++cleared; }
#define tc_sync_msg(tc,msg) ((void)(tc))
static unsigned tc_improve_map_buffer_flags(struct threaded_context *tc,
      struct threaded_resource *r, unsigned usage, int x, int width)
{ (void)tc;(void)r;(void)x;(void)width;return usage; }
static void tc_buffer_disable_cpu_storage(struct pipe_resource *r)
{ (void)r;abort(); }
static void *align_malloc(size_t size, size_t alignment)
{ (void)alignment;void *p=malloc(size);assert(p);memset(p,0xcc,size);++allocations;return p; }
static void align_free(void *p) { assert(p);--allocations;free(p); }
static void u_box_1d(int x, int width, struct pipe_box *b) { b->x=x;b->width=width; }
static void *slab_zalloc(unsigned *pool)
{ (void)pool;return calloc(1,sizeof(struct threaded_transfer)); }
static void slab_free(unsigned *pool, void *p) { (void)pool;free(p); }
static void u_upload_alloc_ref(void *u,unsigned a,unsigned b,unsigned c,
                              unsigned *d,void **e,void **f)
{ (void)u;(void)a;(void)b;(void)c;(void)d;(void)e;(void)f;abort(); }
static bool util_ranges_intersect(struct range *r,unsigned a,unsigned b)
{ return a<r->end && r->start<b; }
static void util_range_add(struct pipe_resource *r,struct range *range,unsigned a,unsigned b)
{ (void)r;(void)range;(void)a;(void)b;abort(); }
static void *fake_map(struct pipe_context *p,struct pipe_resource *r,unsigned level,
                     unsigned usage,const struct pipe_box *box,struct pipe_transfer **out)
{
   (void)p;(void)level;(void)usage;++maps;assert(r==expected_resource);
   if(fail_map)return NULL; /* Gallium requires leaving *out untouched. */
   driver_transfer.b.box=*box;*out=&driver_transfer.b;return gpu_data+box->x;
}
static void fake_unmap(struct pipe_context *p,struct pipe_transfer *transfer)
{ (void)p;assert(transfer==&driver_transfer.b);++unmaps; }
"""
MAIN = r"""
int main(int argc,char **argv)
{
   assert(argc==2);int mode=atoi(argv[1]);
   struct pipe_context pipe={.buffer_map=fake_map,.buffer_unmap=fake_unmap};
   struct threaded_context tc={.pipe=&pipe,.map_buffer_alignment=16};
   struct threaded_resource r={.b={.width0=128},.valid_buffer_range={16,64}};
   struct pipe_resource latest={.width0=128};r.latest=&latest;expected_resource=&latest;
   struct pipe_transfer *sentinel=(void *)(uintptr_t)0x103,*transfer=sentinel;
   struct pipe_box box={.x=24,.width=16};
   for(unsigned i=0;i<sizeof(gpu_data);++i)gpu_data[i]=(unsigned char)(i^0xa5);
   unsigned usage=PIPE_MAP_READ;
   bool unsync=mode==1||mode==4;
   if(unsync)usage|=PIPE_MAP_UNSYNCHRONIZED|TC_TRANSFER_MAP_THREADED_UNSYNC;
   r.allow_cpu_storage=mode==2||mode==5||mode==6;
   fail_map=mode<=2||mode==6;
   void *ret=tc_buffer_map(&tc.base,&r.b,0,usage,&box,&transfer);
   assert(depth==0 && maps==1);
   assert(entered==(unsync?0:1) && cleared==entered);
   if(fail_map) {
      assert(ret==NULL && transfer==sentinel && unmaps==0);
      assert(r.cpu_storage==NULL && allocations==0);
      assert(r.valid_buffer_range.start==16 && r.valid_buffer_range.end==64);
      if(mode==6) {
         assert(r.allow_cpu_storage);fail_map=0;
         ret=tc_buffer_map(&tc.base,&r.b,0,usage,&box,&transfer);
         assert(ret && maps==2 && unmaps==1 && entered==2 && cleared==2 && depth==0);
      } else { puts("PASS");return 0; }
   }
   assert(ret && transfer!=sentinel);
   assert(threaded_transfer(transfer)->valid_buffer_range==&r.valid_buffer_range);
   assert(threaded_transfer(transfer)->cpu_storage_mapped==r.allow_cpu_storage);
   assert(memcmp(ret,gpu_data+box.x,box.width)==0);
   if(r.allow_cpu_storage) {
      assert(unmaps==1 && allocations==1);
      assert(memcmp((char *)r.cpu_storage+16,gpu_data+16,48)==0);
      align_free(r.cpu_storage);free(transfer);
   } else { assert(ret==gpu_data+box.x && unmaps==0 && allocations==0); }
   puts("PASS");return 0;
}
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--revision", help="Mesa git revision instead of working source")
    args = parser.parse_args()
    source = (subprocess.check_output(
        ["git", "show", f"{args.revision}:{SOURCE}"], cwd=MESA, text=True
    ) if args.revision else (MESA / SOURCE).read_text())
    start = source.index("static void *\ntc_buffer_map(")
    end = source.index("\nstatic void *\ntc_texture_map(", start)
    cases = ["direct failure", "unsynchronized failure", "CPU copy failure",
             "direct success", "unsynchronized success", "CPU copy success",
             "CPU copy retry after failure"]
    failed = []
    with tempfile.TemporaryDirectory(prefix="helios-tc-map-") as directory:
        path = Path(directory)
        (path / "test.c").write_text(PRELUDE + source[start:end] + MAIN)
        subprocess.run([os.environ.get("CC", "cc"), "-std=c11", "-O1", "-g",
                        "-Wall", "-Wextra", "-Werror", "-fsanitize=address,undefined",
                        str(path / "test.c"), "-o", str(path / "test")], check=True)
        for i, case in enumerate(cases):
            result = subprocess.run([str(path / "test"), str(i)], capture_output=True, text=True)
            print(f"{'FAIL' if result.returncode else 'PASS'}: {case}")
            if result.returncode:
                failed.append(case)
                print(result.stderr)
    return bool(failed)


if __name__ == "__main__":
    raise SystemExit(main())
