#include <errno.h>
#include <stdio.h>
#include <unistd.h>

static const char *kPath = "/l2a_owner_probe";
void debug_print(const char *msg);

int main(void) {
    if (unlink(kPath) == 0) {
        debug_print("ownerprobe: FAIL unexpected unlink success");
        printf("ownerprobe: FAIL unexpected unlink success path=%s\n", kPath);
        return 1;
    }

    if (errno == EPERM || errno == EACCES) {
        debug_print("ownerprobe: PASS permission denied");
        printf("ownerprobe: PASS permission denied path=%s errno=%d\n", kPath, errno);
        return 0;
    }

    debug_print("ownerprobe: FAIL wrong errno");
    printf("ownerprobe: FAIL wrong errno path=%s errno=%d\n", kPath, errno);
    return 2;
}
