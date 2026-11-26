// A minimal Wayland client for the virtual KWin test. It maps a parent, its
// transient dialog, and an unrelated top-level window with plain shm buffers,
// then stays connected until the test kills it.

#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <wayland-client.h>

#include "xdg-shell-client-protocol.h"

#define WIDTH 240
#define HEIGHT 160

struct state {
  struct wl_display *display;
  struct wl_registry *registry;
  struct wl_compositor *compositor;
  struct wl_shm *shm;
  struct xdg_wm_base *wm_base;
};

struct window {
  struct wl_surface *surface;
  struct xdg_surface *xdg_surface;
  struct xdg_toplevel *toplevel;
  struct wl_buffer *buffer;
  int configured;
};

static void buffer_release(void *data, struct wl_buffer *buffer) {
  (void)data;
  (void)buffer;
}

static const struct wl_buffer_listener buffer_listener = {
    .release = buffer_release,
};

static struct wl_buffer *make_buffer(struct state *state) {
  const int stride = WIDTH * 4;
  const int size = stride * HEIGHT;
  int fd = memfd_create("shisa-wayland-test", MFD_CLOEXEC);
  if (fd < 0) {
    perror("memfd_create");
    exit(1);
  }
  if (ftruncate(fd, size) < 0) {
    perror("ftruncate");
    exit(1);
  }
  void *data = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
  if (data == MAP_FAILED) {
    perror("mmap");
    exit(1);
  }
  memset(data, 0xff, size);
  struct wl_shm_pool *pool = wl_shm_create_pool(state->shm, fd, size);
  struct wl_buffer *buffer = wl_shm_pool_create_buffer(
      pool, 0, WIDTH, HEIGHT, stride, WL_SHM_FORMAT_XRGB8888);
  wl_buffer_add_listener(buffer, &buffer_listener, NULL);
  wl_shm_pool_destroy(pool);
  munmap(data, size);
  close(fd);
  return buffer;
}

static void xdg_surface_configure(void *data, struct xdg_surface *xdg_surface,
                                  uint32_t serial) {
  struct window *window = data;
  xdg_surface_ack_configure(xdg_surface, serial);
  if (window->configured) return;
  wl_surface_attach(window->surface, window->buffer, 0, 0);
  wl_surface_damage_buffer(window->surface, 0, 0, WIDTH, HEIGHT);
  wl_surface_commit(window->surface);
  window->configured = 1;
}

static const struct xdg_surface_listener xdg_surface_listener = {
    .configure = xdg_surface_configure,
};

static void toplevel_configure(void *data, struct xdg_toplevel *toplevel,
                               int32_t width, int32_t height,
                               struct wl_array *states) {
  (void)data;
  (void)toplevel;
  (void)width;
  (void)height;
  (void)states;
}

static void toplevel_close(void *data, struct xdg_toplevel *toplevel) {
  (void)data;
  (void)toplevel;
}

static void toplevel_configure_bounds(void *data,
                                      struct xdg_toplevel *toplevel,
                                      int32_t width, int32_t height) {
  (void)data;
  (void)toplevel;
  (void)width;
  (void)height;
}

static void toplevel_wm_capabilities(void *data, struct xdg_toplevel *toplevel,
                                     struct wl_array *capabilities) {
  (void)data;
  (void)toplevel;
  (void)capabilities;
}

static const struct xdg_toplevel_listener toplevel_listener = {
    .configure = toplevel_configure,
    .close = toplevel_close,
    .configure_bounds = toplevel_configure_bounds,
    .wm_capabilities = toplevel_wm_capabilities,
};

static void wm_base_ping(void *data, struct xdg_wm_base *wm_base,
                         uint32_t serial) {
  (void)data;
  xdg_wm_base_pong(wm_base, serial);
}

static const struct xdg_wm_base_listener wm_base_listener = {
    .ping = wm_base_ping,
};

static void shm_format(void *data, struct wl_shm *shm, uint32_t format) {
  (void)data;
  (void)shm;
  (void)format;
}

static const struct wl_shm_listener shm_listener = {
    .format = shm_format,
};

static void make_window(struct state *state, struct window *window,
                        const char *title, const char *app_id,
                        struct xdg_toplevel *parent) {
  window->surface = wl_compositor_create_surface(state->compositor);
  window->xdg_surface =
      xdg_wm_base_get_xdg_surface(state->wm_base, window->surface);
  xdg_surface_add_listener(window->xdg_surface, &xdg_surface_listener, window);
  window->toplevel = xdg_surface_get_toplevel(window->xdg_surface);
  xdg_toplevel_add_listener(window->toplevel, &toplevel_listener, window);
  xdg_toplevel_set_title(window->toplevel, title);
  if (app_id && app_id[0]) xdg_toplevel_set_app_id(window->toplevel, app_id);
  if (parent) xdg_toplevel_set_parent(window->toplevel, parent);
  window->buffer = make_buffer(state);
  wl_surface_commit(window->surface);
}

static void registry_global(void *data, struct wl_registry *registry,
                            uint32_t name, const char *interface,
                            uint32_t version) {
  struct state *state = data;
  if (strcmp(interface, wl_compositor_interface.name) == 0) {
    uint32_t bind = version < 4 ? version : 4;
    state->compositor =
        wl_registry_bind(registry, name, &wl_compositor_interface, bind);
  } else if (strcmp(interface, wl_shm_interface.name) == 0) {
    state->shm = wl_registry_bind(registry, name, &wl_shm_interface, 1);
    wl_shm_add_listener(state->shm, &shm_listener, state);
  } else if (strcmp(interface, xdg_wm_base_interface.name) == 0) {
    state->wm_base = wl_registry_bind(registry, name, &xdg_wm_base_interface, 1);
    xdg_wm_base_add_listener(state->wm_base, &wm_base_listener, state);
  }
}

static void registry_global_remove(void *data, struct wl_registry *registry,
                                   uint32_t name) {
  (void)data;
  (void)registry;
  (void)name;
}

static const struct wl_registry_listener registry_listener = {
    .global = registry_global,
    .global_remove = registry_global_remove,
};

int main(void) {
  struct state state = {0};
  state.display = wl_display_connect(NULL);
  if (!state.display) {
    fprintf(stderr, "cannot connect to %s\n",
            getenv("WAYLAND_DISPLAY") ? getenv("WAYLAND_DISPLAY") : "Wayland");
    return 1;
  }
  state.registry = wl_display_get_registry(state.display);
  wl_registry_add_listener(state.registry, &registry_listener, &state);
  wl_display_roundtrip(state.display);
  if (!state.compositor || !state.shm || !state.wm_base) {
    fprintf(stderr, "missing compositor, shm or xdg_wm_base\n");
    return 1;
  }

  struct window parent = {0};
  struct window dialog = {0};
  struct window unrelated = {0};
  make_window(&state, &parent, "Shisa Test Parent", "test.parent", NULL);
  make_window(&state, &dialog, "Shisa Test Dialog", "", parent.toplevel);
  make_window(&state, &unrelated, "Shisa Test Unrelated", "test.unrelated", NULL);
  wl_display_roundtrip(state.display);
  printf("ready\n");
  fflush(stdout);

  while (wl_display_dispatch(state.display) != -1) {
  }
  return 0;
}
