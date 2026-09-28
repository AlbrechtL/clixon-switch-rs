/*
 * CLI plugin of clixon-switch: the commands that talk to the user instead of
 * editing the data model. "password" reads passwords without echo and sends
 * the set-password RPC, "factory-reset" asks for confirmation and sends the
 * factory-reset RPC.
 *
 * clixon_cli loads it from CLICON_CLI_DIR; the clispec names the callbacks.
 * C rather than Rust: the CLI API is a separate set of callbacks, and a
 * second Rust library would carry its own copy of std into the flash.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <termios.h>

#include <cligen/cligen.h>
#include <clixon/clixon.h>
#include <clixon/clixon_cli.h>

#define SWITCH_NS "urn:github:albrechtl:clixon-switch"

/* Longest line read; the YANG model allows 128 characters. */
#define LINE_MAX_LEN 512

/* new-password's minimum length in the YANG model. */
#define PASSWORD_MIN_LEN 8

/*! Prints a prompt and reads a line without echo (when stdin is a terminal)
 *
 * @param[in]  prompt  Printed first
 * @param[out] buf     The line, without its newline
 * @param[in]  len     Size of buf
 * @retval     0       OK
 * @retval    -1       End of input
 */
static int
read_secret(const char *prompt,
            char       *buf,
            size_t      len)
{
    struct termios old;
    struct termios noecho;
    int            tty = isatty(STDIN_FILENO) && tcgetattr(STDIN_FILENO, &old) == 0;
    char          *line;

    /* Echo off before the prompt, and TCSANOW rather than TCSAFLUSH: what
     * is typed as soon as the prompt shows must neither be echoed nor be
     * thrown away. */
    if (tty){
        noecho = old;
        noecho.c_lflag &= ~ECHO;
        noecho.c_lflag |= ICANON;
        tcsetattr(STDIN_FILENO, TCSANOW, &noecho);
    }
    fputs(prompt, stdout);
    fflush(stdout);
    line = fgets(buf, len, stdin);
    if (tty){
        tcsetattr(STDIN_FILENO, TCSANOW, &old);
        fputs("\n", stdout);
    }
    if (line == NULL)
        return -1;
    buf[strcspn(buf, "\r\n")] = '\0';
    return 0;
}

/*! Whether the admin password still has to be set (system/state/setup-required)
 *
 * @retval  1   Yes
 * @retval  0   No
 * @retval -1   Error
 */
static int
setup_required(clixon_handle h)
{
    int    retval = -1;
    cvec  *nsc = NULL;
    cxobj *xret = NULL;
    cxobj *x;
    char  *body;

    if ((nsc = xml_nsctx_init("sw", SWITCH_NS)) == NULL)
        goto done;
    if (clicon_rpc_get(h, "/sw:system/sw:state/sw:setup-required", nsc,
                       CONTENT_NONCONFIG, -1, NULL, &xret) < 0)
        goto done;
    if ((x = xpath_first(xret, NULL, "//rpc-error")) != NULL){
        clixon_err_netconf(h, OE_NETCONF, 0, x, "Get setup-required");
        goto done;
    }
    x = xpath_first(xret, NULL, "//setup-required");
    body = x ? xml_body(x) : NULL;
    retval = body != NULL && strcmp(body, "true") == 0;
 done:
    if (xret)
        xml_free(xret);
    if (nsc)
        cvec_free(nsc);
    return retval;
}

/*! Sends an RPC of the clixon-switch module and prints its error, if any
 *
 * @param[in]  h      Clixon handle
 * @param[in]  name   RPC name
 * @param[in]  input  Input leaves as XML, already escaped, or ""
 * @retval     0      OK: <ok/>
 * @retval    -1      Error, printed
 */
static int
switch_rpc(clixon_handle h,
           const char   *name,
           const char   *input)
{
    int    retval = -1;
    cxobj *xtop = NULL;
    cxobj *xret = NULL;
    cxobj *xerr;
    char  *msg;

    if (clixon_xml_parse_va(YB_NONE, NULL, &xtop, NULL,
                            "<rpc xmlns=\"%s\" username=\"%s\" %s>"
                            "<%s xmlns=\"%s\">%s</%s></rpc>",
                            NETCONF_BASE_NAMESPACE,
                            clicon_username_get(h),
                            NETCONF_MESSAGE_ID_ATTR,
                            name, SWITCH_NS, input, name) < 0)
        goto done;
    if (clicon_rpc_netconf_xml(h, xml_child_i(xtop, 0), &xret, NULL) < 0)
        goto done;
    if ((xerr = xpath_first(xret, NULL, "//rpc-error")) != NULL){
        msg = xml_find_body(xerr, "error-message");
        fprintf(stderr, "%s\n", msg ? msg : "failed");
        goto done;
    }
    retval = 0;
 done:
    if (xret)
        xml_free(xret);
    if (xtop)
        xml_free(xtop);
    return retval;
}

/*! Appends <name>value</name>, escaped, to cb */
static int
append_leaf(cbuf       *cb,
            const char *name,
            const char *value)
{
    cprintf(cb, "<%s>", name);
    if (xml_chardata_cbuf_append(cb, 0, value) < 0)
        return -1;
    cprintf(cb, "</%s>", name);
    return 0;
}

/*! CLI callback: change the admin password
 *
 * Asks for the current password, unless the first-login setup is pending,
 * and the new one twice. The backend checks the rules and the current
 * password.
 * @retval  0   Password changed
 * @retval -1   Not changed; the reason is printed
 */
int
switch_password(clixon_handle h,
                cvec         *cvv,
                cvec         *argv)
{
    int   retval = -1;
    int   setup;
    char  current[LINE_MAX_LEN] = "";
    char  new1[LINE_MAX_LEN];
    char  new2[LINE_MAX_LEN];
    cbuf *cb = NULL;

    if ((setup = setup_required(h)) < 0)
        goto done;
    if (setup)
        fprintf(stdout, "Set the admin password. It is used for SSH, the serial console and the web interface.\n");
    else if (read_secret("Current password: ", current, sizeof(current)) < 0)
        goto done;
    if (read_secret("New password: ", new1, sizeof(new1)) < 0)
        goto done;
    if (read_secret("Repeat new password: ", new2, sizeof(new2)) < 0)
        goto done;
    if (strcmp(new1, new2) != 0){
        fprintf(stderr, "The passwords do not match.\n");
        goto done;
    }
    /* The backend checks it too; this only words the common case better
     * than the YANG length error would. strlen() counts bytes, which are
     * at least as many as characters, so the maximum is left to YANG. */
    if (strlen(new1) < PASSWORD_MIN_LEN){
        fprintf(stderr, "The password must have at least %d characters.\n",
                PASSWORD_MIN_LEN);
        goto done;
    }
    if ((cb = cbuf_new()) == NULL)
        goto done;
    if (!setup && append_leaf(cb, "current-password", current) < 0)
        goto done;
    if (append_leaf(cb, "new-password", new1) < 0)
        goto done;
    if (switch_rpc(h, "set-password", cbuf_get(cb)) < 0)
        goto done;
    fprintf(stdout, "Password changed.\n");
    retval = 0;
 done:
    explicit_bzero(current, sizeof(current));
    explicit_bzero(new1, sizeof(new1));
    explicit_bzero(new2, sizeof(new2));
    if (cb){
        explicit_bzero(cbuf_get(cb), cbuf_len(cb));
        cbuf_free(cb);
    }
    return retval;
}

/*! CLI callback: factory reset after confirmation
 *
 * @retval  0   Reset started, or cancelled
 * @retval -1   Error, printed
 */
int
switch_factory_reset(clixon_handle h,
                     cvec         *cvv,
                     cvec         *argv)
{
    char answer[16];

    fprintf(stdout, "All settings, the admin password, the SSH host keys and the HTTPS certificate\n"
            "will be erased, and the switch reboots. Continue? [y/N] ");
    fflush(stdout);
    if (fgets(answer, sizeof(answer), stdin) == NULL ||
        (answer[0] != 'y' && answer[0] != 'Y')){
        fprintf(stdout, "Cancelled.\n");
        return 0;
    }
    if (switch_rpc(h, "factory-reset", "") < 0)
        return -1;
    fprintf(stdout, "Rebooting. The switch comes back with the factory settings.\n");
    return 0;
}

static clixon_plugin_api api = {
    .ca_name = "clixon-switch-cli",
    .ca_init = clixon_plugin_init,
};

clixon_plugin_api *
clixon_plugin_init(clixon_handle h)
{
    return &api;
}
