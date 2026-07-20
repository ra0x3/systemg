FROM sysg-usecase-base

COPY tests/docker/usecase/skip-plain/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/skip-plain/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
