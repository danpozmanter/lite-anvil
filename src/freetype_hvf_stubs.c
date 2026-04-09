/*
 * Stub implementations for FreeType's HVF (Hardware Variable Fonts) symbols.
 *
 * Some pre-built libfreetype.a archives on macOS are compiled with HVF
 * support enabled (hvf.c calls these functions) but the HVF implementation
 * object is missing from the archive. Providing no-op stubs lets the link
 * succeed; variable font rendering falls back to FreeType's software path.
 */
#include <stddef.h>

void  HVF_clear_part_cache(void *a)                          { (void)a; }
void  HVF_close_part_renderer(void *a)                       { (void)a; }
int   HVF_open_part_renderer(void *a, void *b)               { (void)a; (void)b; return 1; /* error */ }
size_t HVF_part_renderer_storage_size(void)                   { return 0; }
int   HVF_render_current_part(void *a, void *b)              { (void)a; (void)b; return 1; }
int   HVF_render_part_axis_count(void *a)                    { (void)a; return 0; }
void  HVF_set_axis_value(void *a, int b, int c)              { (void)a; (void)b; (void)c; }
void  HVF_set_render_part(void *a, int b)                    { (void)a; (void)b; }
void  HVF_refresh_axis_coordinates(void *a)                  { (void)a; }
