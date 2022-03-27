/* --- LICENCE ---
* UPLDIS is licenced under the GNU GPLv3
*
* This program is free software: you can redistribute it and/or modify
* it under the terms of the GNU General Public License as published by
* the Free Software Foundation, either version 3 of the License, or
* (at your option) any later version.
*
* This program is distributed in the hope that it will be useful,
* but WITHOUT ANY WARRANTY; without even the implied warranty of
* MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
* GNU General Public License for more details.
*
* You should have received a copy of the GNU General Public License
* along with this program.  If not, see <http://www.gnu.org/licenses/>.
 */

package main

import (
	"bytes"
	//"encoding/base64"
	"fmt"
	//virustotal "github.com/dutchcoders/go-virustotal"
	"html/template"
	"io/ioutil"
	"log"
	"math/rand"
	"net/http"
	"os/exec"
	"regexp"
	"strings"
	"time"

	"github.com/gorilla/mux"
	"github.com/peterbourgon/diskv"
	md "github.com/shurcooL/github_flavored_markdown"
)

/* --- config --- */
const (
	/* --- url settings ---  */
	formVal      = "p" // the value the upload form uses. ie; 'p=<-'
	minPasteSize = 16
	maxPasteSize = 1024 * 1024 * 1024                                               // 32 MB
	urlLength    = 4                                                                // charlength of the url
	urlCharset   = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789" // available characters the url can use

	/* --- database settings --- */
	basePath  = "pastes"          // dir that the db is located in
	cacheSize = 128 * 1024 * 1024 // 128 MB

	/* --- server settings --- */
	useSSL      = true
	httpsPort   = 8443                 // ssl port
	sslCertPath = "cert/fullchain.cer" // ssl cert
	sslKeyPath  = "cert/upld.info.key" // ssl priv key
	httpPort    = 8080                 // http port
	bindAddress = ""                   // bind address
)

var usageText = struct {
	index, static, temp, md string
}{
	index: `
    Custom urls are also available. Simply append a sub directory to the URL
    and use upld.is exactly like before! The custom urls will also
    automatically generate the usage page.
    ( EXAMPLE: upld.is/custom/ )
    `,
	static: `
    Static file server. Serves files with the correct metadata for the file.
    `,
	temp: `
    /temp/ is a temporary paste subdir. Once viewed, (a single time), pastes
    are wiped from the server, never to be seen again.
    `,
	md: `
    /md/ is a Github flavored markdown parser. Files uploaded here will be
    rendered in html using the same syntax as Github's markdown.
    `,
}

/* if you host your own I'd appreciate a to mention comp.st
*  Need to make the title dynamic etc */
const standardUsageText = `
<!doctype html>
<html>
<head>
<title>{{.BaseURL}}{{.SubDir}} - command line pastebin and more</title>
<style>
a {
	text-decoration: none;
	color: #2196F3;
}
body {
	background-color: #263238;
	color: #fff;
}
.textareaContainer {
	display: block;
	padding: 0;
}
textarea {
	background-color: #455A64;
	color: #fff;
	width: 100%;
	margin: 0;
	padding: 0;
	border-width: 0;
}
</style>

</head>
<body>
<pre>
{{.BaseURL}}(1)                          UPLD.IS                          {{.BaseURL}}(1)

NAME
    {{.BaseURL}}{{.SubDir}} - command line pastebin and more

SYNOPSIS
    #File Upload
    curl {{.BaseURL}}{{.SubDir}} -T &lt;file path&gt;

    # Command output
    &lt;command&gt; | curl {{.BaseURL}}{{.SubDir}} -F{{.FormVal}}=\&lt;-

DESCRIPTION
    A simple, no bullshit command line pastebin. Pastes are created using HTTP
    POST requests. The url is then returned and can be accessed from there.
    {{.DirUsage}}
    Add <a href='http://pygments.org/docs/lexers/'>?&lt;lang&gt;</a> to resulting url for line numbers and syntax highlighting

INSTALL
    Add this to your shell's .rc for an easy to use alias. 
    Usage: upld &lt;file_path&gt;
    
    alias upld='f(){ curl {{.BaseURL}} -T $1; unset -f f; }; f'

EXAMPLE
    $ echo '{{.BaseURL}} is awesome!' | curl {{.BaseURL}}{{.SubDir}} -F{{.FormVal}}=\&lt;-
      {{.BaseURL}}{{.SubDir}}/TEST
    $ curl {{.BaseURL}}{{.SubDir}}/TEST
      {{.BaseURL}} is awesome!
    $ # FILE UPLOAD
    $ curl {{.BaseURL}}{{.SubDir}} -T filename.txt
      {{.BaseURL}}{{.SubDir}}/0x0x.txt
    $ # ALIAS
    $ upld filename.txt
      {{.BaseURL}}/1x1x.txt

UNIQUE SUBDIRS
    <a href="https://{{.BaseURL}}/static/">{{.BaseURL}}/static/</a> is a static file server.
    <a href="https://{{.BaseURL}}/temp/">{{.BaseURL}}/temp/</a> is for single use pastes.
    <a href="https://{{.BaseURL}}/md/">{{.BaseURL}}/md/</a> Github flavored markdown parser.

SEE ALSO
    {{.BaseURL}} is a free service brought to you by oss, (c) 2017

WEB UPLOAD
</pre>
    <form action="https://{{.BaseURL}}{{.SubDir}}" method="POST" accept-charset="UTF-8"><label class="textareaContainer"><textarea name="p" rows="24" placeholder="type paste..."></textarea></label><br><button type="submit">paste it</button></form>
</body>
</html>
`

var reg, _ = regexp.Compile("(\\.[^.]+)$")

// errors n shit
type (
	pasteTooLarge struct{}
	pasteTooSmall struct{}
	pasteNotFound struct{}
	pasteExists   struct{}
)

func (e pasteTooLarge) Error() string {
	return fmt.Sprintf("paste too large (maximum size %d bytes)", maxPasteSize)
}
func (e pasteTooSmall) Error() string { return "paste too small" }
func (e pasteNotFound) Error() string { return "404 not found" }
func (e pasteExists) Error() string   { return "file exists" }

func newID() string {
	urlID := make([]byte, urlLength)
	for i := range urlID {
		urlID[i] = urlCharset[rand.Intn(len(urlCharset))]
	}
	return string(urlID)
}

func flatTransform(s string) []string {
	return []string{}
}

type handler struct {
	disk *diskv.Diskv
}

func readPaste(h *diskv.Diskv, key string) (paste string, err error) {
	var rawPaste []byte
	rawPaste, err = h.Read(key) //key is the paste name
	if err != nil {
		err = pasteNotFound{}
		return
	}
	paste = string(rawPaste)
	return
}

func deletePaste(h *diskv.Diskv, key string) (err error) {
	_, err = h.Read(key) //key is the paste name
	if err != nil {
		err = pasteNotFound{}
		return
	}
	h.Erase(key)
	return
}

func writePaste(h *diskv.Diskv, name string, data []byte) (key string, err error) {
	if len(data) > maxPasteSize {
		err = pasteTooLarge{}
		return
	} else if len(data) < minPasteSize {
		err = pasteTooSmall{}
		return
	}
	name = reg.FindString(name)
	key = newID() + name
	for h.Has(key) {
		key = newID() + name // loop that shit til unique id
	}
	h.Write(key, data)
	return
}

func Highlight(code string, lexer string, key string) (string, error) {
	cmd := exec.Command("pygmentize", "-l"+lexer, "-fhtml", "-O encoding=utf-8,full,style=borland,linenos=table,title="+key) //construct and exec html lexar
	cmd.Stdin = strings.NewReader(code)
	var out bytes.Buffer
	cmd.Stdout = &out
	var stderr bytes.Buffer
	cmd.Stderr = &stderr
	err := cmd.Run()
	return out.String(), err
}

func (h *handler) getCompost(w http.ResponseWriter, req *http.Request) {
	vars := mux.Vars(req)
	j := diskv.New(diskv.Options{
		BasePath:     fmt.Sprintf("%s/_%s", basePath, vars["dir"]),
		Transform:    flatTransform,
		CacheSizeMax: cacheSize,
	})

	if useSSL {
		w.Header().Add("Strict-Transport-Security", "max-age=63072000; includeSubDomains") //ssl lab bullshit
	}
	if vars["file"] != "" {
		paste, err := readPaste(j, vars["file"])
		if err != nil {
			if _, ok := err.(pasteNotFound); ok {
				http.Error(w, "not found", http.StatusNotFound)

			} else {
				http.Error(w, err.Error(), http.StatusInternalServerError)
			}
			log.Printf("[READ ] _%s/%s (error: %s)\n", vars["dir"], vars["file"], err.Error())
			return
		}
		log.Printf("[READ ] _%s/%s\n", vars["dir"], vars["file"])

		var finPaste string
		if vars["dir"] == "md" {
			finPaste = string(md.Markdown([]byte(paste)))
			w.Header().Set("Content-Type", "text/html; charset=utf-8")
		} else if req.URL.RawQuery != "" {
			finPaste, err = Highlight(paste, req.URL.RawQuery, vars["file"])
			w.Header().Set("Content-Type", "text/html; charset=utf-8")
			if err != nil {
				w.Header().Set("Content-Type", "text/plain; charset=utf-8")
				finPaste = paste
			}
		} else {
			w.Header().Set("Content-Type", "text/plain; charset=utf-8") //rewrite this so it isn't fucking shit, I'm disgusted wit u
			finPaste = paste
		}
		fmt.Fprintf(w, "%s", finPaste)
		if vars["dir"] == "temp" {
			deletePaste(j, vars["file"])
		}

		return
	}
}

func (h *handler) post(w http.ResponseWriter, req *http.Request) {
	vars := mux.Vars(req)
	body := req.FormValue(formVal)
	dir := vars["dir"]
	j := diskv.New(diskv.Options{
		BasePath:     fmt.Sprintf("%s/_%s", basePath, dir),
		Transform:    flatTransform,
		CacheSizeMax: cacheSize,
	})

	key, err := writePaste(j, vars["file"], []byte(body))
	if err != nil {
		switch err.(type) {
		case pasteTooLarge, pasteTooSmall:
			http.Error(w, err.Error(), http.StatusNotAcceptable)
		default:
			http.Error(w, err.Error(), http.StatusInternalServerError)
		}
		log.Printf("[WRITE] _%s/%s (error: %s)\n", vars["dir"], vars["file"], err.Error())
		return
	}
	log.Printf("[WRITE] _%s/%s\n", vars["dir"], key)

	if dir != "" {
		dir = dir + "/"
	}
	var scheme string
	if req.TLS != nil {
		scheme = "https://"
	} else {
		scheme = "http://"
	}
	fmt.Fprintf(w, "%s%s/%s%s\n", scheme, req.Host, dir, key)
	return
}

func (h *handler) put(w http.ResponseWriter, req *http.Request) {
	vars := mux.Vars(req)

	body, err := ioutil.ReadAll(req.Body)
	if err != nil {
		fmt.Fprint(w, "an error occurred")
		return
	}

	dir := vars["dir"]
	j := diskv.New(diskv.Options{
		BasePath:     fmt.Sprintf("%s/_%s", basePath, dir),
		Transform:    flatTransform,
		CacheSizeMax: cacheSize,
	})

	key, err := writePaste(j, vars["file"], body)
	if err != nil {
		switch err.(type) {
		case pasteTooLarge, pasteTooSmall:
			http.Error(w, err.Error(), http.StatusNotAcceptable)
		default:
			http.Error(w, err.Error(), http.StatusInternalServerError)
		}
		log.Printf("[WRITE] _%s/%s (error: %s)\n", vars["dir"], vars["file"], err.Error())
		return
	}

	log.Printf("[WRITE] _%s/%s\n", vars["dir"], key)

	if dir != "" {
		dir = dir + "/"
	}
	var scheme string
	if req.TLS != nil {
		scheme = "https://"
	} else {
		scheme = "http://"
	}
	fmt.Fprintf(w, "%s%s/%s%s\n", scheme, req.Host, dir, key)
	return
}

func (h *handler) usage(w http.ResponseWriter, req *http.Request) {
	vars := mux.Vars(req)
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	tmpl, err := template.New("usage").Parse(standardUsageText)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	trailingSlash := (map[bool]string{true: "/", false: ""})[vars["dir"] != ""]
	subDir := trailingSlash + vars["dir"]
	baseURL := req.Host
	var dirUsage string
	switch vars["dir"] {
	case "temp":
		dirUsage = usageText.temp
	case "md":
		dirUsage = usageText.md
	default:
		dirUsage = usageText.index
	}
	data := struct {
		BaseURL  string
		FormVal  string
		DirUsage string
		SubDir   string
	}{baseURL, formVal, dirUsage, subDir}
	_ = tmpl.Execute(w, data)
}

func newHandler() http.Handler {
	h := handler{}
	/* add config for static subdir */
	r := mux.NewRouter().StrictSlash(false)

	r.HandleFunc("/{dir}/", h.usage).Methods("GET")
	r.PathPrefix("/static/").Handler(http.StripPrefix("/static/", http.FileServer(http.Dir(fmt.Sprintf("%s/_static", basePath))))).Methods("GET")
	r.PathPrefix("/.well-known/").Handler(http.StripPrefix("/.well-known/", http.FileServer(http.Dir(".well-known")))) // letsencrypt
	r.HandleFunc("/{dir}/{file}", h.getCompost).Methods("GET")
	r.HandleFunc("/{file}", h.getCompost).Methods("GET")
	r.HandleFunc("/", h.usage).Methods("GET")

	r.HandleFunc("/{dir}/{file}", h.post).Methods("POST")
	r.HandleFunc("/{dir}/", h.post).Methods("POST")
	r.HandleFunc("/{dir}", h.post).Methods("POST")
	r.HandleFunc("/", h.post).Methods("POST")

	r.HandleFunc("/{dir}/{file}", h.put).Methods("PUT")
	r.HandleFunc("/{dir}/", h.put).Methods("PUT")
	r.HandleFunc("/{file}", h.put).Methods("PUT")
	r.HandleFunc("/", h.put).Methods("PUT")
	return r
}

func main() {
	rand.Seed(time.Now().UTC().UnixNano())

	http.Handle("/", newHandler())
	if useSSL {
		httpsAddr := fmt.Sprintf("%s:%d", bindAddress, httpsPort)
		go http.ListenAndServeTLS(httpsAddr, sslCertPath, sslKeyPath, nil) //goroutine ssl server alongside other shit
	}
	httpAddr := fmt.Sprintf("%s:%d", bindAddress, httpPort)
	fmt.Print(http.ListenAndServe(httpAddr, nil))
}
