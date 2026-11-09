/* SPDX-License-Identifier: AGPL-3.0-only AND GPL-3.0-or-later
 * Copyright (C) 2026 Godwit contributors
 * Portions ported from Yapfast (https://github.com/yapfast/yapfast),
 * Copyright (C) the Yapfast authors, GPL-3.0-or-later.
 *
 * dav1d's settings and picture structs are public but sizeable; this shim
 * keeps their ABI layout tied to the exact LiveKit headers at build time.
 * This follows dav1d/dav1d.h and Yapfast's native/av1.c. */
#include <dav1d/dav1d.h>
#include <errno.h>
#include <stdlib.h>
#include <string.h>

struct godwit_av1 {
  Dav1dContext *context;
  Dav1dPicture picture;
  int holds;
};

void *godwit_av1_new(unsigned threads) {
  struct godwit_av1 *decoder = calloc(1, sizeof *decoder);
  if (!decoder) return NULL;
  Dav1dSettings settings;
  dav1d_default_settings(&settings);
  settings.n_threads = (int)(threads ? threads : 1);
  settings.max_frame_delay = 1;
  settings.frame_size_limit = 4096u * 4096u;
  settings.all_layers = 0;
  settings.logger.callback = NULL;
  if (dav1d_open(&decoder->context, &settings) < 0) {
    free(decoder);
    return NULL;
  }
  return decoder;
}

void godwit_av1_free(void *opaque) {
  struct godwit_av1 *decoder = opaque;
  if (!decoder) return;
  if (decoder->holds) dav1d_picture_unref(&decoder->picture);
  dav1d_close(&decoder->context);
  free(decoder);
}

int godwit_av1_decode(void *opaque, const unsigned char *data, size_t size,
                      int *width, int *height, unsigned char **y,
                      unsigned char **u, unsigned char **v,
                      int *y_stride, int *uv_stride) {
  struct godwit_av1 *decoder = opaque;
  if (!decoder || !data || !size) return -1;
  if (decoder->holds) {
    dav1d_picture_unref(&decoder->picture);
    decoder->holds = 0;
  }
  Dav1dData unit;
  uint8_t *copy = dav1d_data_create(&unit, size);
  if (!copy) return -1;
  memcpy(copy, data, size);
  int sent = dav1d_send_data(decoder->context, &unit);
  int got = dav1d_get_picture(decoder->context, &decoder->picture);
  if (sent == DAV1D_ERR(EAGAIN) && got == 0) sent = dav1d_send_data(decoder->context, &unit);
  dav1d_data_unref(&unit);
  if (got == 0) decoder->holds = 1;
  if (sent < 0 && sent != DAV1D_ERR(EAGAIN)) return -1;
  if (got == DAV1D_ERR(EAGAIN)) return 0;
  if (got < 0) return -1;
  Dav1dPicture *picture = &decoder->picture;
  if (picture->p.bpc != 8 || picture->p.layout != DAV1D_PIXEL_LAYOUT_I420 ||
      picture->p.w > 4096 || picture->p.h > 4096) return -1;
  *width = picture->p.w;
  *height = picture->p.h;
  *y = picture->data[0]; *u = picture->data[1]; *v = picture->data[2];
  *y_stride = (int)picture->stride[0];
  *uv_stride = (int)picture->stride[1];
  return 1;
}
