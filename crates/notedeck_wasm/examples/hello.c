/*
 * Minimal notedeck WASM app.
 *
 * Build:
 *   clang --target=wasm32 -nostdlib \
 *     -Wl,--no-entry -Wl,--export=nd_update -Wl,--allow-undefined \
 *     -o hello.wasm hello.c
 */

#include "../api/notedeck_api.h"

static int count = 0;

/* simple int-to-string, returns length written */
static int itoa_simple(int n, char *buf, int buf_size) {
    if (buf_size < 2) return 0;
    if (n == 0) { buf[0] = '0'; return 1; }

    int neg = 0;
    if (n < 0) { neg = 1; n = -n; }

    int len = 0;
    char tmp[16];
    while (n > 0 && len < 16) {
        tmp[len++] = '0' + (n % 10);
        n /= 10;
    }
    if (neg && len < 16) tmp[len++] = '-';

    if (len > buf_size) len = buf_size;
    for (int i = 0; i < len; i++) {
        buf[i] = tmp[len - 1 - i];
    }
    return len;
}

void nd_update(void) {
    nd_heading("Hello from WASM!", 16);
    nd_add_space(8.0f);

    if (nd_button("Click me", 8)) {
        count++;
    }

    nd_add_space(4.0f);

    char buf[48];
    /* "Clicks: " prefix */
    buf[0]='C'; buf[1]='l'; buf[2]='i'; buf[3]='c';
    buf[4]='k'; buf[5]='s'; buf[6]=':'; buf[7]=' ';
    int num_len = itoa_simple(count, buf + 8, 40);
    nd_label(buf, 8 + num_len);
}
