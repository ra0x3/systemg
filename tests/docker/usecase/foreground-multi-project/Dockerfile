FROM sysg-usecase-base

COPY tests/docker/usecase/foreground-multi-project/foo.yaml /usecase/foo.yaml
COPY tests/docker/usecase/foreground-multi-project/bar.yaml /usecase/bar.yaml
COPY tests/docker/usecase/foreground-multi-project/fg2.py /usecase/fg2.py
COPY tests/docker/usecase/foreground-multi-project/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
