FROM sysg-usecase-base

COPY tests/docker/usecase/foreground-boot-grace/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/foreground-boot-grace/fgpty.py /usecase/fgpty.py
COPY tests/docker/usecase/foreground-boot-grace/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
