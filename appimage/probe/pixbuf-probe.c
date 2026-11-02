/* Prints what gdk-pixbuf actually has registered, so a bundle's icon problems can
 * be attributed to the loader cache rather than guessed at. */
#include <gdk-pixbuf/gdk-pixbuf.h>

int main(int argc, char **argv) {
    GSList *formats = gdk_pixbuf_get_formats();
    int count = 0;
    for (GSList *l = formats; l; l = l->next) {
        GdkPixbufFormat *f = l->data;
        gchar *name = gdk_pixbuf_format_get_name(f);
        gchar **mimes = gdk_pixbuf_format_get_mime_types(f);
        g_print("format: %s (mime: %s)\n", name, mimes && mimes[0] ? mimes[0] : "-");
        g_strfreev(mimes);
        g_free(name);
        count++;
    }
    g_slist_free(formats);
    g_print("total formats: %d\n", count);

    if (argc > 1) {
        GError *err = NULL;
        GdkPixbuf *pb = gdk_pixbuf_new_from_file(argv[1], &err);
        if (pb) {
            g_print("load %s: OK %dx%d\n", argv[1],
                    gdk_pixbuf_get_width(pb), gdk_pixbuf_get_height(pb));
        } else {
            g_print("load %s: FAILED %s\n", argv[1], err->message);
        }
    }
    return 0;
}
