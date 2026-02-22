#ifndef NOTEDECK_API_H
#define NOTEDECK_API_H

/*
 * Notedeck WASM API — Stable Interface
 *
 * Stability guarantees:
 *   - Function signatures will never change once published
 *   - New functions may be added; existing ones will not be removed
 *   - All parameters are i32, f32, or (const char *, int) byte buffers
 *   - Extended versions use the _ex suffix if needed
 *
 * WASM module requirements:
 *   - Must export: void nd_update(void)
 *   - Must export: memory (1+ pages)
 *   - Optional exports: nd_app_name_ptr (i32), nd_app_name_len (i32)
 */

/* Text & widgets */
void nd_label(const char *text, int len);
void nd_heading(const char *text, int len);
int  nd_button(const char *text, int len);   /* returns 1 if clicked (prev frame) */

/* Layout */
void nd_add_space(float pixels);

#endif /* NOTEDECK_API_H */
